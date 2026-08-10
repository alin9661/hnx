//! Semantic Ratatui themes and strict custom-theme loading.

use std::{fmt, fs, path::Path, str::FromStr as _};

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use thiserror::Error;

/// The built-in theme families.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinTheme {
    #[default]
    Classic,
    Midnight,
    #[serde(alias = "ansi16", alias = "16-color", alias = "16color")]
    Ansi16,
    #[serde(alias = "none")]
    NoColor,
}

impl BuiltinTheme {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Midnight => "midnight",
            Self::Ansi16 => "ansi-16",
            Self::NoColor => "no-color",
        }
    }
}

impl fmt::Display for BuiltinTheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for BuiltinTheme {
    type Err = ThemeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "classic" => Ok(Self::Classic),
            "midnight" | "dark" => Ok(Self::Midnight),
            "ansi-16" | "ansi16" | "16-color" | "16color" => Ok(Self::Ansi16),
            "no-color" | "nocolor" | "none" => Ok(Self::NoColor),
            _ => Err(ThemeError::UnknownBuiltin(value.to_owned())),
        }
    }
}

/// A complete semantic color palette used by the terminal UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Theme {
    pub name: String,
    pub background: Color,
    pub foreground: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_fg: Color,
    pub highlight: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub border: Color,
    pub selected_fg: Color,
    pub selected_bg: Color,
    pub link: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::classic()
    }
}

impl Theme {
    /// Hacker News' familiar light palette with an orange accent.
    #[must_use]
    pub fn classic() -> Self {
        Self {
            name: "classic".to_owned(),
            background: Color::Rgb(247, 246, 240),
            foreground: Color::Rgb(0, 0, 0),
            muted: Color::Rgb(112, 112, 106),
            accent: Color::Rgb(255, 102, 0),
            accent_fg: Color::Black,
            highlight: Color::Rgb(196, 72, 0),
            success: Color::Rgb(26, 127, 55),
            warning: Color::Rgb(158, 92, 0),
            error: Color::Rgb(190, 35, 35),
            border: Color::Rgb(183, 183, 174),
            selected_fg: Color::Rgb(0, 0, 0),
            selected_bg: Color::Rgb(229, 228, 222),
            link: Color::Rgb(26, 82, 160),
        }
    }

    /// A low-glare true-color theme for dark terminals.
    #[must_use]
    pub fn midnight() -> Self {
        Self {
            name: "midnight".to_owned(),
            background: Color::Rgb(13, 17, 23),
            foreground: Color::Rgb(230, 237, 243),
            muted: Color::Rgb(139, 148, 158),
            accent: Color::Rgb(255, 126, 59),
            accent_fg: Color::Black,
            highlight: Color::Rgb(88, 166, 255),
            success: Color::Rgb(63, 185, 80),
            warning: Color::Rgb(210, 153, 34),
            error: Color::Rgb(248, 81, 73),
            border: Color::Rgb(48, 54, 61),
            selected_fg: Color::White,
            selected_bg: Color::Rgb(31, 111, 235),
            link: Color::Rgb(88, 166, 255),
        }
    }

    /// A portable palette restricted to the terminal's basic 16 colors.
    #[must_use]
    pub fn ansi16() -> Self {
        Self {
            name: "ansi-16".to_owned(),
            background: Color::Black,
            foreground: Color::Gray,
            muted: Color::DarkGray,
            accent: Color::LightYellow,
            accent_fg: Color::Black,
            highlight: Color::LightCyan,
            success: Color::LightGreen,
            warning: Color::Yellow,
            error: Color::LightRed,
            border: Color::DarkGray,
            selected_fg: Color::White,
            selected_bg: Color::Blue,
            link: Color::LightBlue,
        }
    }

    /// A palette that emits no explicit colors, suitable for `NO_COLOR`.
    #[must_use]
    pub fn no_color() -> Self {
        Self {
            name: "no-color".to_owned(),
            background: Color::Reset,
            foreground: Color::Reset,
            muted: Color::Reset,
            accent: Color::Reset,
            accent_fg: Color::Reset,
            highlight: Color::Reset,
            success: Color::Reset,
            warning: Color::Reset,
            error: Color::Reset,
            border: Color::Reset,
            selected_fg: Color::Reset,
            selected_bg: Color::Reset,
            link: Color::Reset,
        }
    }

    #[must_use]
    pub fn builtin(theme: BuiltinTheme) -> Self {
        match theme {
            BuiltinTheme::Classic => Self::classic(),
            BuiltinTheme::Midnight => Self::midnight(),
            BuiltinTheme::Ansi16 => Self::ansi16(),
            BuiltinTheme::NoColor => Self::no_color(),
        }
    }

    /// Resolves a built-in name such as `classic`, `midnight`, or `ansi-16`.
    ///
    /// # Errors
    ///
    /// Returns [`ThemeError::UnknownBuiltin`] when the name is not recognized.
    pub fn named(name: &str) -> Result<Self, ThemeError> {
        Ok(Self::builtin(name.parse()?))
    }

    /// Loads a custom TOML file from an explicit, testable path.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or its theme is invalid.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ThemeError> {
        let contents = fs::read_to_string(path)?;
        Self::from_toml(&contents)
    }

    /// Parses a custom theme.
    ///
    /// The file may select `extends = "classic"`, `"midnight"`, or
    /// `"ansi-16"`; semantic overrides live in a `[colors]` table. Colors may
    /// be names, `#RRGGBB`, indices from 0 through 255, or RGB arrays.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid TOML, unknown fields, invalid colors, or an
    /// unsafe theme name.
    pub fn from_toml(contents: &str) -> Result<Self, ThemeError> {
        let file: ThemeFile = toml::from_str(contents)?;
        let mut theme = Self::builtin(file.extends.unwrap_or_default());

        if let Some(name) = file.name {
            let name = name.trim();
            if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                return Err(ThemeError::InvalidName);
            }
            name.clone_into(&mut theme.name);
        }

        macro_rules! apply_color {
            ($field:ident) => {
                if let Some(value) = file.colors.$field {
                    theme.$field = value.parse(stringify!($field))?;
                }
            };
        }

        apply_color!(background);
        apply_color!(foreground);
        apply_color!(muted);
        let accent_overridden = file.colors.accent.is_some();
        apply_color!(accent);
        apply_color!(accent_fg);
        if let Some(value) = file.colors.highlight {
            theme.highlight = value.parse("highlight")?;
        } else if accent_overridden {
            theme.highlight = theme.accent;
        }
        apply_color!(success);
        apply_color!(warning);
        apply_color!(error);
        apply_color!(border);
        apply_color!(selected_fg);
        apply_color!(selected_bg);
        apply_color!(link);

        Ok(theme)
    }

    #[must_use]
    pub fn base_style(&self) -> Style {
        Style::default().fg(self.foreground).bg(self.background)
    }

    #[must_use]
    pub fn primary_style(&self) -> Style {
        self.base_style().add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn muted_style(&self) -> Style {
        self.base_style().fg(self.muted).add_modifier(Modifier::DIM)
    }

    #[must_use]
    pub fn accent_style(&self) -> Style {
        self.base_style()
            .fg(self.highlight)
            .add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn selected_style(&self) -> Style {
        Style::default()
            .fg(self.selected_fg)
            .bg(self.selected_bg)
            .add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn link_style(&self) -> Style {
        self.base_style()
            .fg(self.link)
            .add_modifier(Modifier::UNDERLINED)
    }

    /// Uses semantic palette roles for nested-comment rails.
    #[must_use]
    pub fn depth_style(&self, depth: usize) -> Style {
        let color = match depth % 5 {
            0 => self.accent,
            1 => self.link,
            2 => self.success,
            3 => self.warning,
            _ => self.muted,
        };
        self.base_style().fg(color)
    }

    #[must_use]
    pub fn success_style(&self) -> Style {
        self.base_style()
            .fg(self.success)
            .add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn warning_style(&self) -> Style {
        self.base_style()
            .fg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn error_style(&self) -> Style {
        self.base_style()
            .fg(self.error)
            .add_modifier(Modifier::BOLD)
    }
}

/// Errors loading or parsing a theme.
#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("theme file could not be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("theme TOML is invalid: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("unknown built-in theme `{0}`")]
    UnknownBuiltin(String),
    #[error("theme name must contain 1-64 printable characters")]
    InvalidName,
    #[error("invalid color for `{field}`: {value}")]
    InvalidColor { field: &'static str, value: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    extends: Option<BuiltinTheme>,
    #[serde(default)]
    colors: ColorOverrides,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ColorOverrides {
    background: Option<ColorValue>,
    foreground: Option<ColorValue>,
    muted: Option<ColorValue>,
    accent: Option<ColorValue>,
    accent_fg: Option<ColorValue>,
    highlight: Option<ColorValue>,
    success: Option<ColorValue>,
    warning: Option<ColorValue>,
    error: Option<ColorValue>,
    border: Option<ColorValue>,
    selected_fg: Option<ColorValue>,
    selected_bg: Option<ColorValue>,
    link: Option<ColorValue>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ColorValue {
    Text(String),
    Index(u8),
    Rgb([u8; 3]),
}

impl ColorValue {
    fn parse(self, field: &'static str) -> Result<Color, ThemeError> {
        match self {
            Self::Index(index) => Ok(Color::Indexed(index)),
            Self::Rgb([red, green, blue]) => Ok(Color::Rgb(red, green, blue)),
            Self::Text(value) => {
                Color::from_str(&value).map_err(|_| ThemeError::InvalidColor { field, value })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_have_stable_semantic_palettes() {
        assert_eq!(Theme::default(), Theme::classic());
        let classic = Theme::classic();
        assert_eq!(classic.background, Color::Rgb(247, 246, 240));
        assert_eq!(classic.foreground, Color::Rgb(0, 0, 0));
        assert_eq!(classic.accent, Color::Rgb(255, 102, 0));
        assert_eq!(classic.accent_fg, Color::Black);
        assert_eq!(classic.highlight, Color::Rgb(196, 72, 0));
        assert_eq!(classic.selected_fg, Color::Rgb(0, 0, 0));
        assert_eq!(classic.selected_bg, Color::Rgb(229, 228, 222));
        assert_eq!(classic.accent_style().fg, Some(classic.highlight));
        assert_eq!(classic.selected_style().fg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(classic.selected_style().bg, Some(classic.selected_bg));
        assert!(
            classic
                .primary_style()
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(
            Theme::named("dark").expect("theme resolves"),
            Theme::midnight()
        );
        assert_eq!(Theme::midnight().accent_fg, Color::Black);
        let ansi = Theme::ansi16();
        assert!(matches!(ansi.accent, Color::LightYellow));
        assert_eq!(ansi.accent_fg, Color::Black);
        assert!(!matches!(
            ansi.background,
            Color::Rgb(..) | Color::Indexed(_)
        ));
        let none = Theme::named("none").expect("no-color theme resolves");
        assert_eq!(none, Theme::no_color());
        assert_eq!(none.accent_fg, Color::Reset);
        assert_eq!(none.selected_bg, Color::Reset);
        assert!(none.muted_style().add_modifier.contains(Modifier::DIM));
        assert!(none.accent_style().add_modifier.contains(Modifier::BOLD));
        assert!(
            none.link_style()
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn custom_toml_extends_and_overrides_semantic_colors() {
        let theme = Theme::from_toml(
            r##"
                name = "ocean"
                extends = "midnight"

                [colors]
                background = "#001122"
                accent = [10, 20, 30]
                accent_fg = "white"
                selected_bg = 24
            "##,
        )
        .expect("theme parses");

        assert_eq!(theme.name, "ocean");
        assert_eq!(theme.background, Color::Rgb(0, 17, 34));
        assert_eq!(theme.accent, Color::Rgb(10, 20, 30));
        assert_eq!(theme.accent_fg, Color::White);
        assert_eq!(theme.highlight, theme.accent);
        assert_eq!(theme.selected_bg, Color::Indexed(24));
        assert_eq!(theme.foreground, Theme::midnight().foreground);
    }

    #[test]
    fn explicit_highlight_wins_over_the_custom_accent_fallback() {
        let theme = Theme::from_toml(
            r##"
                [colors]
                accent = "#ff6600"
                highlight = "#c44800"
            "##,
        )
        .expect("theme parses");

        assert_eq!(theme.accent, Color::Rgb(255, 102, 0));
        assert_eq!(theme.highlight, Color::Rgb(196, 72, 0));
        assert_eq!(theme.accent_style().fg, Some(theme.highlight));
    }

    #[test]
    fn custom_theme_path_is_caller_selected() {
        let directory = tempfile::tempdir().expect("tempdir creates");
        let path = directory.path().join("chosen-name.toml");
        fs::write(
            &path,
            "name = 'portable'\nextends = '16-color'\n[colors]\naccent = 'cyan'\n",
        )
        .expect("theme writes");

        let theme = Theme::load(path).expect("theme loads");
        assert_eq!(theme.name, "portable");
        assert_eq!(theme.accent, Color::Cyan);
        assert_eq!(theme.highlight, Color::Cyan);
    }

    #[test]
    fn unknown_fields_and_bad_colors_are_errors() {
        assert!(Theme::from_toml("[colors]\naccent = 'not-a-color'").is_err());
        assert!(Theme::from_toml("[colors]\nacccent = 'red'").is_err());
    }
}
