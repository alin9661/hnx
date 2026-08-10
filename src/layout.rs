//! Validated pane preferences and responsive geometry.

use std::{fmt, str::FromStr};

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::app::{FocusPane, SecondaryPane};

pub const MIN_PANE_PERCENT: u8 = 15;
pub const MIN_PANE_COLUMNS: u16 = 18;

/// The pane arrangement requested by the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneMode {
    #[default]
    Two,
    Three,
}

impl PaneMode {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Two => Self::Three,
            Self::Three => Self::Two,
        }
    }
}

impl fmt::Display for PaneMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Two => "two",
            Self::Three => "three",
        })
    }
}

impl FromStr for PaneMode {
    type Err = LayoutError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "two" => Ok(Self::Two),
            "three" => Ok(Self::Three),
            _ => Err(LayoutError::InvalidMode(value.to_owned())),
        }
    }
}

/// Persistable layout preferences after validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutPreferences {
    pub mode: PaneMode,
    pub two: [u8; 2],
    pub three: [u8; 3],
    pub two_min_width: u16,
    pub three_min_width: u16,
}

impl Default for LayoutPreferences {
    fn default() -> Self {
        Self {
            mode: PaneMode::Two,
            two: [44, 56],
            three: [38, 34, 28],
            two_min_width: 80,
            three_min_width: 120,
        }
    }
}

impl LayoutPreferences {
    /// Checks ratios and breakpoint ordering atomically.
    pub fn validate(self) -> Result<Self, LayoutError> {
        validate_ratios("two", &self.two)?;
        validate_ratios("three", &self.three)?;
        if self.two_min_width == 0 {
            return Err(LayoutError::ZeroBreakpoint("two_min_width"));
        }
        if self.three_min_width < self.two_min_width {
            return Err(LayoutError::BreakpointOrder {
                two: self.two_min_width,
                three: self.three_min_width,
            });
        }
        Ok(self)
    }

    #[must_use]
    pub const fn with_mode(mut self, mode: PaneMode) -> Self {
        self.mode = mode;
        self
    }

    /// Resizes the focused pane by percentage points while retaining a 15% minimum.
    pub fn resized(&self, focus: FocusPane, delta: i8) -> Result<Self, LayoutError> {
        let mut next = self.clone();
        match self.mode {
            PaneMode::Two => {
                let left_delta = if focus == FocusPane::Stories {
                    delta
                } else {
                    -delta
                };
                adjust_pair(&mut next.two, 0, 1, left_delta)?;
            }
            PaneMode::Three => match focus {
                FocusPane::Stories => adjust_pair(&mut next.three, 0, 1, delta)?,
                FocusPane::Thread => adjust_pair(&mut next.three, 1, 2, delta)?,
                FocusPane::Detail => adjust_pair(&mut next.three, 2, 1, delta)?,
            },
        }
        next.validate()
    }
}

fn validate_ratios<const N: usize>(
    name: &'static str,
    ratios: &[u8; N],
) -> Result<(), LayoutError> {
    if ratios.iter().any(|ratio| *ratio < MIN_PANE_PERCENT) {
        return Err(LayoutError::RatioTooSmall {
            name,
            minimum: MIN_PANE_PERCENT,
        });
    }
    let total: u16 = ratios.iter().map(|ratio| u16::from(*ratio)).sum();
    if total != 100 {
        return Err(LayoutError::RatioTotal { name, total });
    }
    Ok(())
}

fn adjust_pair<const N: usize>(
    ratios: &mut [u8; N],
    focused: usize,
    neighbor: usize,
    delta: i8,
) -> Result<(), LayoutError> {
    let focused_value = i16::from(ratios[focused]) + i16::from(delta);
    let neighbor_value = i16::from(ratios[neighbor]) - i16::from(delta);
    if focused_value < i16::from(MIN_PANE_PERCENT) || neighbor_value < i16::from(MIN_PANE_PERCENT) {
        return Err(LayoutError::ResizeRejected(MIN_PANE_PERCENT));
    }
    ratios[focused] =
        u8::try_from(focused_value).map_err(|_| LayoutError::ResizeRejected(MIN_PANE_PERCENT))?;
    ratios[neighbor] =
        u8::try_from(neighbor_value).map_err(|_| LayoutError::ResizeRejected(MIN_PANE_PERCENT))?;
    Ok(())
}

/// The visible pane set recorded by the renderer for navigation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneSet(u8);

impl PaneSet {
    const STORIES: u8 = 1;
    const THREAD: u8 = 2;
    const DETAIL: u8 = 4;

    #[must_use]
    pub const fn one(pane: FocusPane) -> Self {
        Self(Self::bit(pane))
    }

    #[must_use]
    pub const fn two(secondary: SecondaryPane) -> Self {
        Self(
            Self::STORIES
                | match secondary {
                    SecondaryPane::Thread => Self::THREAD,
                    SecondaryPane::Detail => Self::DETAIL,
                },
        )
    }

    #[must_use]
    pub const fn three() -> Self {
        Self(Self::STORIES | Self::THREAD | Self::DETAIL)
    }

    #[must_use]
    pub const fn contains(self, pane: FocusPane) -> bool {
        self.0 & Self::bit(pane) != 0
    }

    #[must_use]
    pub fn ordered(self) -> impl DoubleEndedIterator<Item = FocusPane> {
        [FocusPane::Stories, FocusPane::Thread, FocusPane::Detail]
            .into_iter()
            .filter(move |pane| self.contains(*pane))
    }

    const fn bit(pane: FocusPane) -> u8 {
        match pane {
            FocusPane::Stories => Self::STORIES,
            FocusPane::Thread => Self::THREAD,
            FocusPane::Detail => Self::DETAIL,
        }
    }
}

/// The responsive arrangement actually rendered at the current width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedMode {
    One,
    Two,
    Three,
}

/// Rectangles used by the renderer. `None` means a hidden pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneLayout {
    pub mode: ResolvedMode,
    pub panes: PaneSet,
    pub stories: Option<Rect>,
    pub thread: Option<Rect>,
    pub detail: Option<Rect>,
}

/// Resolves requested preferences into safe pane rectangles.
#[must_use]
pub fn resolve_panes(
    area: Rect,
    preferences: &LayoutPreferences,
    focus: FocusPane,
    secondary: SecondaryPane,
) -> PaneLayout {
    if preferences.mode == PaneMode::Three && area.width >= preferences.three_min_width {
        let rects = split(area, &preferences.three);
        if rects.iter().all(|rect| rect.width >= MIN_PANE_COLUMNS) {
            return PaneLayout {
                mode: ResolvedMode::Three,
                panes: PaneSet::three(),
                stories: Some(rects[0]),
                thread: Some(rects[1]),
                detail: Some(rects[2]),
            };
        }
    }

    if area.width >= preferences.two_min_width {
        let rects = split(area, &preferences.two);
        if rects.iter().all(|rect| rect.width >= MIN_PANE_COLUMNS) {
            let (thread, detail) = match secondary {
                SecondaryPane::Thread => (Some(rects[1]), None),
                SecondaryPane::Detail => (None, Some(rects[1])),
            };
            return PaneLayout {
                mode: ResolvedMode::Two,
                panes: PaneSet::two(secondary),
                stories: Some(rects[0]),
                thread,
                detail,
            };
        }
    }

    PaneLayout {
        mode: ResolvedMode::One,
        panes: PaneSet::one(focus),
        stories: (focus == FocusPane::Stories).then_some(area),
        thread: (focus == FocusPane::Thread).then_some(area),
        detail: (focus == FocusPane::Detail).then_some(area),
    }
}

fn split<const N: usize>(area: Rect, ratios: &[u8; N]) -> [Rect; N] {
    let mut x = area.x;
    let mut remaining = area.width;
    std::array::from_fn(|index| {
        let width = if index + 1 == N {
            remaining
        } else {
            area.width.saturating_mul(u16::from(ratios[index])) / 100
        };
        let rect = Rect::new(x, area.y, width, area.height);
        x = x.saturating_add(width);
        remaining = remaining.saturating_sub(width);
        rect
    })
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("unknown layout mode `{0}`")]
    InvalidMode(String),
    #[error("layout `{name}` ratios must total 100, got {total}")]
    RatioTotal { name: &'static str, total: u16 },
    #[error("layout `{name}` ratios must each be at least {minimum}%")]
    RatioTooSmall { name: &'static str, minimum: u8 },
    #[error("layout breakpoint `{0}` must be greater than zero")]
    ZeroBreakpoint(&'static str),
    #[error("three_min_width ({three}) must be at least two_min_width ({two})")]
    BreakpointOrder { two: u16, three: u16 },
    #[error("pane resize rejected; every pane must remain at least {0}%")]
    ResizeRejected(u8),
}
