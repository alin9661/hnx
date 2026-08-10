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
            match (mode, ratios.as_slice()) {
                (PaneMode::Two, [stories, secondary]) => {
                    preferences.two = [*stories, *secondary];
                }
                (PaneMode::Three, [stories, thread, detail]) => {
                    preferences.three = [*stories, *thread, *detail];
                }
                (mode, ratios) => {
                    return Err(LayoutError::OverrideArity {
                        mode: *mode,
                        expected: match mode {
                            PaneMode::Two => 2,
                            PaneMode::Three => 3,
                        },
                        actual: ratios.len(),
                    });
                }
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
    Ok(load_discovered_baseline(&path))
}

fn load_discovered_baseline(path: &Path) -> (LayoutPreferences, Option<String>) {
    if !path.exists() {
        return (LayoutPreferences::default(), None);
    }
    match read_config(path) {
        Ok(preferences) => (preferences, None),
        Err(_) => (
            LayoutPreferences::default(),
            Some("Invalid auto-discovered layout config; using built-in defaults".to_owned()),
        ),
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

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _};

    use tempfile::NamedTempFile;

    use super::*;

    fn config(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("temp config");
        file.write_all(contents.as_bytes()).expect("write config");
        file
    }

    #[test]
    fn cli_syntax_parses_modes_ratios_and_reset() {
        assert_eq!("reset".parse(), Ok(LayoutOverride::Reset));
        assert_eq!(
            "two:40".parse(),
            Ok(LayoutOverride::Apply {
                mode: PaneMode::Two,
                ratios: Some(vec![40, 60]),
            })
        );
        assert_eq!(
            "three:35,30".parse(),
            Ok(LayoutOverride::Apply {
                mode: PaneMode::Three,
                ratios: Some(vec![35, 30, 35]),
            })
        );
        assert!("two:10".parse::<LayoutOverride>().is_err());
        assert!("three:40".parse::<LayoutOverride>().is_err());
        assert!("three:60,50".parse::<LayoutOverride>().is_err());
    }

    #[test]
    fn programmatic_override_rejects_bad_arity_without_panicking() {
        let override_ = LayoutOverride::Apply {
            mode: PaneMode::Three,
            ratios: Some(vec![50]),
        };

        assert!(matches!(
            override_.apply(&LayoutPreferences::default()),
            Err(LayoutError::OverrideArity {
                mode: PaneMode::Three,
                expected: 3,
                actual: 1
            })
        ));
    }

    #[test]
    fn precedence_is_cli_then_stored_then_toml_then_defaults() {
        let file = config(
            "[layout]\nmode = 'three'\ntwo = [40, 60]\nthree = [34, 33, 33]\ntwo_min_width = 70\nthree_min_width = 100\n",
        );
        let stored = LayoutPreferences {
            mode: PaneMode::Two,
            two: [45, 55],
            ..LayoutPreferences::default()
        };
        let stored_json = serde_json::to_string(&stored).expect("stored JSON");

        let toml_only = resolve_layout(Some(file.path()), None, None).expect("TOML resolves");
        assert_eq!(toml_only.active.mode, PaneMode::Three);
        assert_eq!(toml_only.active.two, [40, 60]);

        let stored_result =
            resolve_layout(Some(file.path()), Some(&stored_json), None).expect("stored resolves");
        assert_eq!(stored_result.active, stored);
        assert_eq!(stored_result.baseline.two, [40, 60]);

        let cli = "three:38,32".parse::<LayoutOverride>().expect("CLI parses");
        let cli_result = resolve_layout(Some(file.path()), Some(&stored_json), Some(&cli))
            .expect("CLI resolves");
        assert_eq!(cli_result.active.mode, PaneMode::Three);
        assert_eq!(cli_result.active.three, [38, 32, 30]);
        assert_eq!(cli_result.active.two, [45, 55]);
        assert!(cli_result.persist_cli);
    }

    #[test]
    fn reset_ignores_stored_state_and_restores_toml() {
        let file = config("[layout]\nmode = 'three'\ntwo = [42, 58]\n");
        let stored = serde_json::to_string(&LayoutPreferences {
            two: [50, 50],
            ..LayoutPreferences::default()
        })
        .expect("stored JSON");
        let result = resolve_layout(
            Some(file.path()),
            Some(&stored),
            Some(&LayoutOverride::Reset),
        )
        .expect("reset resolves");
        assert_eq!(result.active.two, [42, 58]);
        assert!(result.reset_saved);
        assert!(!result.persist_cli);
    }

    #[test]
    fn corrupt_stored_state_falls_back_atomically_with_one_warning() {
        let file = config("[layout]\ntwo = [40, 60]\n");
        let result = resolve_layout(Some(file.path()), Some("{broken"), None)
            .expect("stored corruption is recoverable");
        assert_eq!(result.active.two, [40, 60]);
        assert_eq!(
            result.warning.as_deref(),
            Some("Saved layout was invalid; using config or built-in defaults")
        );

        let invalid = serde_json::json!({
            "mode": "two",
            "two": [90, 10],
            "three": [38, 34, 28],
            "two_min_width": 80,
            "three_min_width": 120
        });
        let result = resolve_layout(Some(file.path()), Some(&invalid.to_string()), None)
            .expect("stored validation failure is recoverable");
        assert_eq!(result.active.two, [40, 60]);
        assert!(result.warning.is_some());
    }

    #[test]
    fn explicit_config_errors_are_fatal() {
        let missing = Path::new("/definitely/missing/hnx-config.toml");
        assert!(matches!(
            resolve_layout(Some(missing), None, None),
            Err(ConfigError::Read { .. })
        ));
        let invalid = config("[layout]\ntwo = [10, 90]\n");
        assert!(matches!(
            resolve_layout(Some(invalid.path()), None, None),
            Err(ConfigError::Layout(_))
        ));
    }

    #[test]
    fn discovered_config_is_silent_when_missing_and_warns_once_when_corrupt() {
        let directory = tempfile::tempdir().expect("temp config directory");
        let path = directory.path().join("config.toml");
        let (missing, warning) = load_discovered_baseline(&path);
        assert_eq!(missing, LayoutPreferences::default());
        assert!(warning.is_none());

        fs::write(&path, "[layout]\ntwo = [40, 60]\n").expect("write valid config");
        let (valid, warning) = load_discovered_baseline(&path);
        assert_eq!(valid.two, [40, 60]);
        assert!(warning.is_none());

        fs::write(&path, "[layout]\ntwo = [10, 90]\n").expect("write invalid config");
        let (fallback, warning) = load_discovered_baseline(&path);
        assert_eq!(fallback, LayoutPreferences::default());
        assert_eq!(
            warning.as_deref(),
            Some("Invalid auto-discovered layout config; using built-in defaults")
        );
    }
}
