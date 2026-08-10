//! Sanitization helpers for untrusted text and links.
//!
//! Hacker News content is user supplied.  These helpers keep terminal control
//! sequences out of Ratatui output and ensure links handed to a browser or the
//! article fetcher use an expected, externally routable HTTP(S) URL.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use thiserror::Error;
use url::{Host, Url};

const ESC: char = '\u{001b}';
const CSI: char = '\u{009b}';
const DCS: char = '\u{0090}';
const OSC: char = '\u{009d}';
const SOS: char = '\u{0098}';
const PM: char = '\u{009e}';
const APC: char = '\u{009f}';
const ST: char = '\u{009c}';

/// Controls which layout characters survive [`sanitize_text_with_options`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SanitizeOptions {
    /// Retain line breaks, normalizing CR and CRLF to LF.
    pub preserve_newlines: bool,
    /// Retain horizontal tabs.
    pub preserve_tabs: bool,
}

impl Default for SanitizeOptions {
    fn default() -> Self {
        Self {
            preserve_newlines: true,
            preserve_tabs: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Ground,
    Escape,
    ControlSequence,
    OperatingSystemCommand,
    ControlString,
    EscapeInOperatingSystemCommand,
    EscapeInControlString,
}

/// Removes ANSI escape sequences and C0/C1 control characters from text.
///
/// Newlines and tabs are retained by default. CRLF and lone CR are normalized
/// to LF so terminal rendering cannot be manipulated with carriage returns.
#[must_use]
pub fn sanitize_text(input: &str) -> String {
    sanitize_text_with_options(input, SanitizeOptions::default())
}

/// Removes terminal controls using explicit newline and tab handling.
#[must_use]
pub fn sanitize_text_with_options(input: &str, options: SanitizeOptions) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut state = State::Ground;

    while let Some(ch) = chars.next() {
        match state {
            State::Ground => match ch {
                ESC => state = State::Escape,
                CSI => state = State::ControlSequence,
                OSC => state = State::OperatingSystemCommand,
                DCS | SOS | PM | APC => state = State::ControlString,
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        let _ = chars.next();
                    }
                    if options.preserve_newlines {
                        output.push('\n');
                    }
                }
                '\n' if options.preserve_newlines => output.push('\n'),
                '\t' if options.preserve_tabs => output.push('\t'),
                value if is_control(value) => {}
                value => output.push(value),
            },
            State::Escape => match ch {
                '[' => state = State::ControlSequence,
                ']' => state = State::OperatingSystemCommand,
                'P' | 'X' | '^' | '_' => state = State::ControlString,
                // Intermediate bytes can precede the final byte in an ECMA-48
                // escape sequence.
                '\u{0020}'..='\u{002f}' => {}
                '\u{0030}'..='\u{007e}' | ST => state = State::Ground,
                ESC => {}
                value => {
                    state = State::Ground;
                    push_ground_character(&mut output, value, options);
                }
            },
            State::ControlSequence => match ch {
                ESC => state = State::Escape,
                ST | '\u{0040}'..='\u{007e}' => state = State::Ground,
                '\r' | '\n' | '\t' => {
                    // A layout character cannot be part of a valid CSI. End a
                    // malformed sequence without swallowing the rest of a line.
                    state = State::Ground;
                    push_ground_character(&mut output, ch, options);
                }
                value if value > '\u{007f}' && !is_control(value) => {
                    state = State::Ground;
                    output.push(value);
                }
                _ => {}
            },
            State::OperatingSystemCommand => match ch {
                '\u{0007}' | ST => state = State::Ground,
                ESC => state = State::EscapeInOperatingSystemCommand,
                _ => {}
            },
            State::ControlString => match ch {
                ST => state = State::Ground,
                ESC => state = State::EscapeInControlString,
                _ => {}
            },
            State::EscapeInOperatingSystemCommand => {
                if ch == '\\' || ch == ST {
                    state = State::Ground;
                } else if ch != ESC {
                    state = State::OperatingSystemCommand;
                }
            }
            State::EscapeInControlString => {
                if ch == '\\' || ch == ST {
                    state = State::Ground;
                } else if ch != ESC {
                    state = State::ControlString;
                }
            }
        }
    }

    output
}

/// Produces display-safe text on one line and collapses whitespace runs.
#[must_use]
pub fn sanitize_single_line(input: &str) -> String {
    sanitize_text(input)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Alias useful at boundaries where the intent is specifically ANSI removal.
#[must_use]
pub fn strip_ansi(input: &str) -> String {
    sanitize_text(input)
}

fn push_ground_character(output: &mut String, ch: char, options: SanitizeOptions) {
    match ch {
        '\r' | '\n' if options.preserve_newlines => output.push('\n'),
        '\t' if options.preserve_tabs => output.push('\t'),
        value if !is_control(value) => output.push(value),
        _ => {}
    }
}

const fn is_control(ch: char) -> bool {
    matches!(ch, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}')
}

/// Policy for validating a URL before handing it to the OS or HTTP client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UrlPolicy {
    /// Permit unencrypted HTTP in addition to HTTPS.
    pub allow_http: bool,
    /// Permit literal private addresses and localhost hostnames.
    ///
    /// This should normally remain `false`; it exists for explicitly trusted
    /// intranets and deterministic local HTTP tests.
    pub allow_private_hosts: bool,
    /// Permit usernames or passwords embedded in a URL.
    pub allow_credentials: bool,
}

impl Default for UrlPolicy {
    fn default() -> Self {
        Self {
            allow_http: true,
            allow_private_hosts: false,
            allow_credentials: false,
        }
    }
}

/// Why an untrusted URL was rejected.
#[derive(Debug, Error)]
pub enum UrlSafetyError {
    /// The value was empty.
    #[error("URL is empty")]
    Empty,
    /// The value contains whitespace or terminal/control characters.
    #[error("URL contains whitespace or control characters")]
    UnsafeCharacters,
    /// URL parsing failed.
    #[error("invalid URL: {0}")]
    Invalid(#[from] url::ParseError),
    /// Only HTTP(S) URLs are accepted.
    #[error("URL scheme `{0}` is not allowed")]
    Scheme(String),
    /// An HTTP(S) URL must include a host.
    #[error("URL does not contain a host")]
    MissingHost,
    /// Embedded credentials are disallowed.
    #[error("URL credentials are not allowed")]
    Credentials,
    /// Local and non-public literal destinations are disallowed.
    #[error("local or non-public URL host is not allowed")]
    PrivateHost,
    /// Port zero cannot be a useful network destination.
    #[error("URL port zero is not allowed")]
    PortZero,
}

/// Validates an externally supplied HTTP(S) URL with the default safe policy.
///
/// # Errors
///
/// Returns [`UrlSafetyError`] when the value is malformed or violates the
/// default external-network policy.
pub fn validate_url(input: &str) -> Result<Url, UrlSafetyError> {
    validate_url_with_policy(input, UrlPolicy::default())
}

/// Validates an externally supplied HTTP(S) URL using an explicit policy.
///
/// # Errors
///
/// Returns [`UrlSafetyError`] when the value is malformed or violates `policy`.
pub fn validate_url_with_policy(input: &str, policy: UrlPolicy) -> Result<Url, UrlSafetyError> {
    if input.is_empty() {
        return Err(UrlSafetyError::Empty);
    }
    if input.chars().any(|ch| ch.is_whitespace() || is_control(ch)) {
        return Err(UrlSafetyError::UnsafeCharacters);
    }

    let url = Url::parse(input)?;
    match url.scheme() {
        "https" => {}
        "http" if policy.allow_http => {}
        scheme => return Err(UrlSafetyError::Scheme(scheme.to_owned())),
    }

    if !policy.allow_credentials && (!url.username().is_empty() || url.password().is_some()) {
        return Err(UrlSafetyError::Credentials);
    }
    if url.port() == Some(0) {
        return Err(UrlSafetyError::PortZero);
    }

    let host = url.host().ok_or(UrlSafetyError::MissingHost)?;
    if !policy.allow_private_hosts && !host_is_public(&host) {
        return Err(UrlSafetyError::PrivateHost);
    }

    Ok(url)
}

/// Returns whether a URL passes [`validate_url`].
#[must_use]
pub fn is_safe_url(input: &str) -> bool {
    validate_url(input).is_ok()
}

fn host_is_public(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let normalized = domain.trim_end_matches('.');
            !normalized.eq_ignore_ascii_case("localhost")
                && !normalized.to_ascii_lowercase().ends_with(".localhost")
        }
        Host::Ipv4(address) => ip_is_public(IpAddr::V4(*address)),
        Host::Ipv6(address) => ip_is_public(IpAddr::V6(*address)),
    }
}

/// Returns whether an already-resolved address is suitable for an external request.
///
/// URL validation alone cannot make this decision for hostnames. Network clients
/// must apply this predicate to the addresses returned by their connector's DNS
/// resolver so validation and connection use the same resolution result.
#[must_use]
pub(crate) fn ip_is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => ipv4_is_public(address),
        IpAddr::V6(address) => ipv6_is_public(address),
    }
}

fn ipv4_is_public(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();

    if matches!(a, 0 | 10 | 127 | 224..=255)
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && ((b == 0 && matches!(c, 0 | 2)) || b == 168))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && ((18..=19).contains(&b) || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113)
    {
        return false;
    }

    true
}

fn ipv6_is_public(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let first = segments[0];
    let is_documentation = (first == 0x2001 && segments[1] == 0x0db8)
        || (first == 0x3fff && segments[1] & 0xf000 == 0);

    // IPv4-mapped IPv6 addresses must obey the IPv4 checks too.
    if segments[..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        let octets = address.octets();
        return ipv4_is_public(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ));
    }

    if address.is_unspecified()
        || address.is_loopback()
        || first & 0xfe00 == 0xfc00 // unique-local fc00::/7
        || first & 0xffc0 == 0xfe80 // link-local fe80::/10
        || first & 0xffc0 == 0xfec0 // deprecated site-local fec0::/10
        || first & 0xff00 == 0xff00 // multicast ff00::/8
        || first & 0xe000 != 0x2000 // allocated global unicast is currently 2000::/3
        || (first == 0x2001 && segments[1] < 0x0200) // IETF protocol assignments
        || is_documentation
    // 2001:db8::/32 and 3fff::/20
    {
        return false;
    }

    // 6to4 embeds the destination IPv4 address in bits 16..48. Applying the
    // IPv4 policy prevents transition routing from reaching a private address.
    if first == 0x2002 {
        let high = segments[1].to_be_bytes();
        let low = segments[2].to_be_bytes();
        return ipv4_is_public(Ipv4Addr::new(high[0], high[1], low[0], low[1]));
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_sgr_sequences() {
        assert_eq!(sanitize_text("plain \x1b[31mred\x1b[0m"), "plain red");
        assert_eq!(sanitize_text("a\u{009b}2Jb"), "ab");
    }

    #[test]
    fn strips_osc_and_control_strings() {
        assert_eq!(
            sanitize_text("a\x1b]8;;https://evil.invalid\x1b\\link\x1b]8;;\x07b"),
            "alinkb"
        );
        assert_eq!(sanitize_text("a\x1bPpayload\x1b\\b"), "ab");
        assert_eq!(sanitize_text("before\x1b]unterminated"), "before");
    }

    #[test]
    fn removes_controls_but_normalizes_layout() {
        assert_eq!(sanitize_text("a\0b\r\nc\rd\te\u{0085}f"), "ab\nc\nd\tef");
        assert_eq!(sanitize_single_line(" a\n\tb  c "), "a b c");
    }

    #[test]
    fn validates_only_public_http_urls_by_default() {
        assert!(validate_url("https://news.ycombinator.com/item?id=1").is_ok());
        assert!(matches!(
            validate_url("javascript:alert(1)"),
            Err(UrlSafetyError::Scheme(_))
        ));
        assert!(matches!(
            validate_url("http://127.0.0.1:3000"),
            Err(UrlSafetyError::PrivateHost)
        ));
        assert!(matches!(
            validate_url("https://user:secret@example.com"),
            Err(UrlSafetyError::Credentials)
        ));
        assert!(matches!(
            validate_url("https://example.com/\nnext"),
            Err(UrlSafetyError::UnsafeCharacters)
        ));
    }

    #[test]
    fn explicit_policy_can_allow_local_test_server() {
        let policy = UrlPolicy {
            allow_private_hosts: true,
            ..UrlPolicy::default()
        };
        assert!(validate_url_with_policy("http://[::1]:4321/a", policy).is_ok());
    }

    #[test]
    fn resolved_address_policy_rejects_non_public_ranges() {
        for address in [
            "0.0.0.0",
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "192.0.2.1",
            "224.0.0.1",
            "::",
            "::1",
            "fd00::1",
            "fe80::1",
            "fec0::1",
            "2001:db8::1",
            "3fff::1",
            "ff02::1",
            "2002:7f00:1::",
        ] {
            let address = address.parse().expect("test address parses");
            assert!(!ip_is_public(address), "accepted non-public {address}");
        }

        for address in ["8.8.8.8", "2606:4700:4700::1111"] {
            let address = address.parse().expect("test address parses");
            assert!(ip_is_public(address), "rejected public {address}");
        }
    }
}
