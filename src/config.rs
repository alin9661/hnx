//! Platform configuration and layout-precedence resolution.

use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::Deserialize;
use thiserror::Error;

use crate::layout::{LayoutError, LayoutPreferences, PaneMode};

/// A validated command-line layout request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutOverride {
    Apply {
        mode: PaneMode,
        ratios: Option<Vec<u8>>,
    },
    Reset,
}

impl LayoutOverride {
    /// Applies the CLI mode and optional ratios over a validated baseline.
    ///
    /// # Errors
    ///
    /// Returns an error if the resulting layout violates ratio or breakpoint invariants.
    pub fn apply(&self, baseline: &LayoutPreferences) -> Result<LayoutPreferences, LayoutError> {
        let Self::Apply { mode, ratios } = self else {
            return Ok(baseline.clone());
        };
        let mut preferences = baseline.clone().with_mode(*mode);
        if let Some(ratios) = ratios {
            match mode {
                PaneMode::Two => preferences.two = [ratios[0], ratios[1]],
                PaneMode::Three => preferences.three = [ratios[0], ratios[1], ratios[2]],
            }
        }
        preferences.validate()
    }
}

impl FromStr for LayoutOverride {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("reset") {
            return Ok(Self::Reset);
        }
        let (mode, values) = value
            .split_once(':')
            .map_or((value, None), |(mode, ratios)| (mode, Some(ratios)));
        let mode = mode
            .parse::<PaneMode>()
            .map_err(|error| error.to_string())?;
        let Some(values) = values else {
            return Ok(Self::Apply { mode, ratios: None });
        };
        let supplied = values
            .split(',')
            .map(|part| {
                part.parse::<u8>()
                    .map_err(|_| "layout ratios must be whole percentages".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ratios = match (mode, supplied.as_slice()) {
            (PaneMode::Two, [stories]) => vec![
                *stories,
                100_u8
                    .checked_sub(*stories)
                    .ok_or_else(|| "two-pane stories percentage must be below 100".to_owned())?,
            ],
            (PaneMode::Three, [stories, thread]) => vec![
                *stories,
                *thread,
                100_u8
                    .checked_sub(*stories)
                    .and_then(|remaining| remaining.checked_sub(*thread))
                    .ok_or_else(|| "three-pane percentages must total below 100".to_owned())?,
            ],
            (PaneMode::Two, _) => {
                return Err("two-pane syntax is `two[:STORIES]`".to_owned());
            }
            (PaneMode::Three, _) => {
                return Err("three-pane syntax is `three[:STORIES,THREAD]`".to_owned());
            }
        };
        let override_ = Self::Apply {
            mode,
            ratios: Some(ratios),
        };
        override_
            .apply(&LayoutPreferences::default())
            .map_err(|error| error.to_string())?;
        Ok(override_)
    }
}

/// Result of precedence resolution plus the baseline restored by `Alt+0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutResolution {
    pub active: LayoutPreferences,
    pub baseline: LayoutPreferences,
    pub warning: Option<String>,
    pub persist_cli: bool,
    pub reset_saved: bool,
}

/// Resolves CLI, stored state, TOML, and defaults in descending precedence.
///
/// # Errors
///
/// Returns an error for an explicitly requested unreadable/invalid config or invalid CLI layout.
pub fn resolve_layout(
    explicit_config: Option<&Path>,
    stored_json: Option<&str>,
    cli: Option<&LayoutOverride>,
) -> Result<LayoutResolution, ConfigError> {
    let (baseline, config_warning) = load_baseline(explicit_config)?;
    if matches!(cli, Some(LayoutOverride::Reset)) {
        return Ok(LayoutResolution {
            active: baseline.clone(),
            baseline,
            warning: config_warning,
            persist_cli: false,
            reset_saved: true,
        });
    }

    let (stored, stored_warning) = stored_json.map_or((None, None), |value| {
        match serde_json::from_str::<LayoutPreferences>(value)
            .map_err(ConfigError::StoredJson)
            .and_then(|preferences| preferences.validate().map_err(ConfigError::Layout))
        {
            Ok(preferences) => (Some(preferences), None),
            Err(_) => (
                None,
                Some("Saved layout was invalid; using config or built-in defaults".to_owned()),
            ),
        }
    });
    let inherited = stored.unwrap_or_else(|| baseline.clone());
    let (active, persist_cli) = if let Some(override_) = cli {
        (override_.apply(&inherited)?, true)
    } else {
        (inherited, false)
    };
    Ok(LayoutResolution {
        active,
        baseline,
        warning: stored_warning.or(config_warning),
        persist_cli,
        reset_saved: false,
    })
}

fn load_baseline(
    explicit: Option<&Path>,
) -> Result<(LayoutPreferences, Option<String>), ConfigError> {
    if let Some(path) = explicit {
        return read_config(path).map(|preferences| (preferences, None));
    }
    let Some(path) = default_config_path() else {
        return Ok((LayoutPreferences::default(), None));
    };
    if !path.exists() {
        return Ok((LayoutPreferences::default(), None));
    }
    match read_config(&path) {
        Ok(preferences) => Ok((preferences, None)),
        Err(_) => Ok((
            LayoutPreferences::default(),
            Some("Invalid auto-discovered layout config; using built-in defaults".to_owned()),
        )),
    }
}

fn read_config(path: &Path) -> Result<LayoutPreferences, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let config: ConfigFile = toml::from_str(&contents).map_err(|source| ConfigError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    config
        .layout
        .unwrap_or_default()
        .validate()
        .map_err(ConfigError::Layout)
}

#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "hnx", "hnx")
        .map(|directories| directories.config_dir().join("config.toml"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    layout: Option<LayoutPreferences>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config `{}`: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("config `{}` is invalid: {source}", path.display())]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid layout configuration: {0}")]
    Layout(#[from] LayoutError),
    #[error("saved layout JSON is invalid: {0}")]
    StoredJson(serde_json::Error),
}
