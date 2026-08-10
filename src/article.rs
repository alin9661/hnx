//! Bounded article fetching and terminal-friendly HTML conversion.

use std::{
    error::Error as StdError,
    net::{SocketAddr, ToSocketAddrs as _},
    sync::LazyLock,
    time::Duration,
};

use futures::StreamExt as _;
use regex::bytes::Regex;
use reqwest::{
    Client, StatusCode,
    dns::{Addrs, Name, Resolve, Resolving},
    header, redirect,
};
use thiserror::Error;
use url::Url;

use crate::sanitize::{
    UrlPolicy, UrlSafetyError, ip_is_public, sanitize_text, validate_url_with_policy,
};

pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_ARTICLE_WIDTH: usize = 88;

/// Limits and network policy used by [`ArticleClient`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArticleConfig {
    pub max_response_bytes: usize,
    pub width: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_redirects: usize,
    pub allow_plain_text: bool,
    /// Allows explicit localhost/private-IP URLs, primarily for local tests.
    pub allow_private_hosts: bool,
    pub user_agent: String,
}

impl Default for ArticleConfig {
    fn default() -> Self {
        Self {
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            width: DEFAULT_ARTICLE_WIDTH,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(20),
            max_redirects: 5,
            allow_plain_text: true,
            allow_private_hosts: false,
            user_agent: concat!("hnx/", env!("CARGO_PKG_VERSION")).to_owned(),
        }
    }
}

/// A fetched article after safe, readable conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Article {
    pub url: Url,
    pub content_type: String,
    pub response_bytes: usize,
    pub text: String,
}

/// Errors returned by article fetching and conversion.
#[derive(Debug, Error)]
pub enum ArticleError {
    #[error("unsafe article URL: {0}")]
    UnsafeUrl(#[from] UrlSafetyError),
    #[error("article HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("article server returned HTTP {0}")]
    HttpStatus(StatusCode),
    #[error("article response did not provide a Content-Type")]
    MissingContentType,
    #[error("article Content-Type `{0}` is not supported")]
    UnsupportedContentType(String),
    #[error(
        "article response exceeds the {limit}-byte limit{advertised}",
        advertised = advertised_size(*advertised)
    )]
    ResponseTooLarge {
        limit: usize,
        advertised: Option<u64>,
    },
    #[error("article could not be converted to text: {0}")]
    Conversion(String),
    #[error("invalid article client configuration: {0}")]
    InvalidConfig(&'static str),
}

pub type ArticleResult<T> = Result<T, ArticleError>;

/// Reusable HTTP client with strict redirect, type, timeout, and size limits.
#[derive(Clone, Debug)]
pub struct ArticleClient {
    http: Client,
    config: ArticleConfig,
}

impl ArticleClient {
    /// Creates a client with conservative production defaults.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new() -> ArticleResult<Self> {
        Self::with_config(ArticleConfig::default())
    }

    /// Creates a client with explicit limits, useful for deterministic tests.
    ///
    /// # Errors
    ///
    /// Returns an error when a limit is invalid or the HTTP client cannot be
    /// constructed.
    pub fn with_config(config: ArticleConfig) -> ArticleResult<Self> {
        Self::with_resolver(config, SystemResolver)
    }

    fn with_resolver<R>(config: ArticleConfig, resolver: R) -> ArticleResult<Self>
    where
        R: Resolve + 'static,
    {
        validate_config(&config)?;
        let url_policy = url_policy(&config);
        let maximum_redirects = config.max_redirects;
        let redirect_policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > maximum_redirects {
                return attempt.error(RedirectGuardError::TooMany);
            }
            if validate_url_with_policy(attempt.url().as_str(), url_policy).is_err() {
                return attempt.error(RedirectGuardError::UnsafeTarget);
            }
            attempt.follow()
        });

        let http = Client::builder()
            .user_agent(&config.user_agent)
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .redirect(redirect_policy)
            // A proxy may resolve the article hostname itself, bypassing this
            // client's address policy. Direct connections keep the guarded
            // resolver authoritative over the socket destination.
            .no_proxy()
            .dns_resolver(GuardedResolver::new(
                resolver,
                config.allow_private_hosts,
            ))
            .build()?;
        Ok(Self { http, config })
    }

    #[must_use]
    pub const fn config(&self) -> &ArticleConfig {
        &self.config
    }

    /// Fetches and converts one HTTP(S) article.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe URL, a network/status failure, an
    /// unsupported content type, an oversized body, or conversion failure.
    pub async fn fetch(&self, input: &str) -> ArticleResult<Article> {
        let policy = url_policy(&self.config);
        let url = validate_url_with_policy(input, policy)?;
        let response = self
            .http
            .get(url)
            .header(
                header::ACCEPT,
                "text/html, application/xhtml+xml;q=0.9, text/plain;q=0.5",
            )
            .send()
            .await?;

        // Validate the final URL too. This also protects callers that supply an
        // HTTP client with behavior changed by a future reqwest release.
        let final_url = validate_url_with_policy(response.url().as_str(), policy)?;
        if !response.status().is_success() {
            return Err(ArticleError::HttpStatus(response.status()));
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .ok_or(ArticleError::MissingContentType)?
            .to_str()
            .map_err(|_| ArticleError::UnsupportedContentType("non-UTF-8 header".to_owned()))?
            .to_owned();
        let content_kind = ContentKind::from_header(&content_type, self.config.allow_plain_text)?;

        let content_length = response.content_length();
        let response_limit = u64::try_from(self.config.max_response_bytes).unwrap_or(u64::MAX);
        if content_length.is_some_and(|length| length > response_limit) {
            return Err(ArticleError::ResponseTooLarge {
                limit: self.config.max_response_bytes,
                advertised: content_length,
            });
        }

        let bytes = read_bounded(response, self.config.max_response_bytes).await?;
        let text = match content_kind {
            ContentKind::Html => {
                let readable_html = strip_non_content_html(&bytes);
                html2text::from_read(readable_html.as_slice(), self.config.width)
                    .map_err(|error| ArticleError::Conversion(error.to_string()))?
            }
            ContentKind::PlainText => String::from_utf8_lossy(&bytes).into_owned(),
        };

        Ok(Article {
            url: final_url,
            content_type,
            response_bytes: bytes.len(),
            text: normalize_text(&sanitize_text(&text)),
        })
    }

    /// Fetches an article and returns only its converted text.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::fetch`].
    pub async fn fetch_text(&self, input: &str) -> ArticleResult<String> {
        Ok(self.fetch(input).await?.text)
    }
}

#[derive(Clone, Copy, Debug)]
struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let lookup_host = host.clone();
            let result = tokio::task::spawn_blocking(move || {
                (lookup_host.as_str(), 0)
                    .to_socket_addrs()
                    .map(std::iter::Iterator::collect::<Vec<_>>)
            })
            .await;

            let addresses = match result {
                Ok(Ok(addresses)) => addresses,
                Ok(Err(error)) => {
                    return Err(resolver_error(DnsGuardError::Lookup {
                        host,
                        error: error.to_string(),
                    }));
                }
                Err(error) => {
                    return Err(resolver_error(DnsGuardError::LookupTask {
                        host,
                        error: error.to_string(),
                    }));
                }
            };
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

#[derive(Clone, Debug)]
struct GuardedResolver<R> {
    inner: R,
    allow_private_hosts: bool,
}

impl<R> GuardedResolver<R> {
    const fn new(inner: R, allow_private_hosts: bool) -> Self {
        Self {
            inner,
            allow_private_hosts,
        }
    }
}

impl<R> Resolve for GuardedResolver<R>
where
    R: Resolve,
{
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let resolving = self.inner.resolve(name);
        let allow_private_hosts = self.allow_private_hosts;
        Box::pin(async move {
            let addresses = resolving.await?.collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(resolver_error(DnsGuardError::NoAddresses { host }));
            }
            if !allow_private_hosts
                && let Some(address) = addresses.iter().find(|address| !ip_is_public(address.ip()))
            {
                return Err(resolver_error(DnsGuardError::NonPublicAddress {
                    host,
                    address: *address,
                }));
            }

            // These are the exact addresses reqwest's connector will attempt.
            // It does not perform a second lookup behind this resolver.
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

type ResolverError = Box<dyn StdError + Send + Sync>;

fn resolver_error(error: DnsGuardError) -> ResolverError {
    Box::new(error)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentKind {
    Html,
    PlainText,
}

impl ContentKind {
    fn from_header(header: &str, allow_plain_text: bool) -> ArticleResult<Self> {
        let essence = header
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match essence.as_str() {
            "text/html" | "application/xhtml+xml" => Ok(Self::Html),
            "text/plain" if allow_plain_text => Ok(Self::PlainText),
            _ => Err(ArticleError::UnsupportedContentType(header.to_owned())),
        }
    }
}

async fn read_bounded(response: reqwest::Response, limit: usize) -> ArticleResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(0)
            .min(limit),
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(ArticleError::ResponseTooLarge {
                limit,
                advertised: None,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn normalize_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut blank_lines = 0_u8;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_lines = blank_lines.saturating_add(1);
            if blank_lines > 2 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        output.push_str(line);
        output.push('\n');
    }
    output.trim().to_owned()
}

fn strip_non_content_html(html: &[u8]) -> Vec<u8> {
    static NON_CONTENT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        ["script", "style", "noscript", "template"]
            .into_iter()
            .map(|element| {
                Regex::new(&format!(
                    r"(?is)<{element}(?:\s[^>]*)?/>|<{element}(?:\s[^>]*)?>.*?</{element}\s*>|<{element}(?:\s[^>]*)?>.*\z"
                ))
                .expect("hard-coded non-content HTML pattern is valid")
            })
            .collect()
    });

    NON_CONTENT.iter().fold(html.to_vec(), |body, pattern| {
        pattern.replace_all(&body, &b""[..]).into_owned()
    })
}

fn validate_config(config: &ArticleConfig) -> ArticleResult<()> {
    if config.max_response_bytes == 0 {
        return Err(ArticleError::InvalidConfig(
            "max_response_bytes must be greater than zero",
        ));
    }
    if config.width < 20 {
        return Err(ArticleError::InvalidConfig(
            "article width must be at least 20 columns",
        ));
    }
    if config.connect_timeout.is_zero() || config.request_timeout.is_zero() {
        return Err(ArticleError::InvalidConfig(
            "network timeouts must be greater than zero",
        ));
    }
    if config.max_redirects == 0 {
        return Err(ArticleError::InvalidConfig(
            "max_redirects must be greater than zero",
        ));
    }
    if config.user_agent.trim().is_empty() {
        return Err(ArticleError::InvalidConfig("user_agent must not be empty"));
    }
    Ok(())
}

const fn url_policy(config: &ArticleConfig) -> UrlPolicy {
    UrlPolicy {
        allow_http: true,
        allow_private_hosts: config.allow_private_hosts,
        allow_credentials: false,
    }
}

fn advertised_size(size: Option<u64>) -> String {
    size.map_or_else(String::new, |size| {
        format!(" (server advertised {size} bytes)")
    })
}

#[derive(Debug, Error)]
enum RedirectGuardError {
    #[error("too many article redirects")]
    TooMany,
    #[error("article redirect target is unsafe")]
    UnsafeTarget,
}

#[derive(Debug, Error)]
enum DnsGuardError {
    #[error("DNS lookup for `{host}` returned no addresses")]
    NoAddresses { host: String },
    #[error("DNS lookup for `{host}` returned non-public address {address}")]
    NonPublicAddress { host: String, address: SocketAddr },
    #[error("DNS lookup for `{host}` failed: {error}")]
    Lookup { host: String, error: String },
    #[error("DNS lookup task for `{host}` failed: {error}")]
    LookupTask { host: String, error: String },
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

    #[derive(Clone, Debug)]
    struct StaticResolver {
        addresses: Vec<SocketAddr>,
        calls: Arc<AtomicUsize>,
    }

    impl StaticResolver {
        fn new(addresses: Vec<SocketAddr>) -> Self {
            Self {
                addresses,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Resolve for StaticResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let addresses = self.addresses.clone();
            Box::pin(async move {
                let addresses: Addrs = Box::new(addresses.into_iter());
                Ok(addresses)
            })
        }
    }

    #[derive(Clone, Debug)]
    struct RebindingResolver {
        first: SocketAddr,
        rebound: SocketAddr,
        calls: Arc<AtomicUsize>,
    }

    impl Resolve for RebindingResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            let address = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first
            } else {
                self.rebound
            };
            Box::pin(async move {
                let addresses: Addrs = Box::new(std::iter::once(address));
                Ok(addresses)
            })
        }
    }

    fn local_config(maximum: usize) -> ArticleConfig {
        ArticleConfig {
            max_response_bytes: maximum,
            allow_private_hosts: true,
            ..ArticleConfig::default()
        }
    }

    #[tokio::test]
    async fn fetches_bounded_html_and_converts_it_to_readable_text() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/story"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        "<html><body><h1>Hello</h1><script>bad()</script><p>Readable story.</p></body></html>",
                        "text/html; charset=utf-8",
                    ),
            )
            .mount(&server)
            .await;

        let client = ArticleClient::with_config(local_config(4_096)).expect("client builds");
        let article = client
            .fetch(&format!("{}/story", server.uri()))
            .await
            .expect("article fetches");

        assert!(article.text.contains("Hello"));
        assert!(article.text.contains("Readable story."));
        assert!(
            !article.text.contains("bad()"),
            "script content leaked into article text: {}",
            article.text
        );
        assert!(article.response_bytes <= 4_096);
    }

    #[tokio::test]
    async fn rejects_unsupported_or_oversized_responses() {
        let server = MockServer::start().await;
        Mock::given(matchers::path("/image"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0_u8; 8], "image/png"))
            .mount(&server)
            .await;
        Mock::given(matchers::path("/large"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("x".repeat(1_024), "text/html"))
            .mount(&server)
            .await;

        let client = ArticleClient::with_config(local_config(64)).expect("client builds");
        assert!(matches!(
            client.fetch(&format!("{}/image", server.uri())).await,
            Err(ArticleError::UnsupportedContentType(_))
        ));
        assert!(matches!(
            client.fetch(&format!("{}/large", server.uri())).await,
            Err(ArticleError::ResponseTooLarge { .. })
        ));
    }

    #[test]
    fn production_client_rejects_private_targets_before_requesting() {
        let client = ArticleClient::new().expect("client builds");
        let runtime = tokio::runtime::Runtime::new().expect("runtime builds");
        assert!(matches!(
            runtime.block_on(client.fetch("http://127.0.0.1:1/article")),
            Err(ArticleError::UnsafeUrl(UrlSafetyError::PrivateHost))
        ));
    }

    #[tokio::test]
    async fn connector_rejects_a_domain_resolved_to_loopback() {
        let server = MockServer::start().await;
        let resolver = StaticResolver::new(vec![*server.address()]);
        let calls = Arc::clone(&resolver.calls);
        let client = ArticleClient::with_resolver(ArticleConfig::default(), resolver)
            .expect("client builds");

        let result = client
            .fetch(&format!(
                "http://article.test:{}/story",
                server.address().port()
            ))
            .await;

        assert!(matches!(result, Err(ArticleError::Http(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            server
                .received_requests()
                .await
                .expect("request recording remains available")
                .is_empty(),
            "the connector reached the private destination"
        );
    }

    #[tokio::test]
    async fn connector_uses_the_exact_validated_resolution() {
        let server = MockServer::start().await;
        Mock::given(matchers::path("/pinned"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("Pinned", "text/plain"))
            .mount(&server)
            .await;
        let resolver = StaticResolver::new(vec![*server.address()]);
        let calls = Arc::clone(&resolver.calls);
        let client =
            ArticleClient::with_resolver(local_config(4_096), resolver).expect("client builds");

        let article = client
            .fetch(&format!(
                "http://article.test:{}/pinned",
                server.address().port()
            ))
            .await
            .expect("the connector uses the custom resolution");

        assert_eq!(article.text, "Pinned");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request recording remains available")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn guarded_resolver_rechecks_every_rebinding_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = RebindingResolver {
            first: "8.8.8.8:0".parse().expect("public address parses"),
            rebound: "127.0.0.1:0".parse().expect("loopback address parses"),
            calls: Arc::clone(&calls),
        };
        let resolver = GuardedResolver::new(resolver, false);

        let first = resolver
            .resolve("article.test".parse().expect("host parses"))
            .await
            .expect("the first public resolution is accepted")
            .collect::<Vec<_>>();
        let rebound = resolver
            .resolve("article.test".parse().expect("host parses"))
            .await;

        assert_eq!(first, vec!["8.8.8.8:0".parse().expect("address parses")]);
        assert!(rebound.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
