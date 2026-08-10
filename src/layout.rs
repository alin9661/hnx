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
        let mode = match self.mode {
            PaneMode::Two => ResolvedMode::Two,
            PaneMode::Three => ResolvedMode::Three,
        };
        self.resized_for(mode, focus, delta)
    }

    /// Resizes the divider in the currently resolved layout.
    pub fn resized_for(
        &self,
        mode: ResolvedMode,
        focus: FocusPane,
        delta: i8,
    ) -> Result<Self, LayoutError> {
        let mut next = self.clone();
        match mode {
            ResolvedMode::One => return Err(LayoutError::ResizeUnavailable),
            ResolvedMode::Two => {
                let left_delta = if focus == FocusPane::Stories {
                    delta
                } else {
                    -delta
                };
                adjust_pair(&mut next.two, 0, 1, left_delta)?;
            }
            ResolvedMode::Three => match focus {
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
    #[error("pane resizing is unavailable while only one pane is visible")]
    ResizeUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(layout: PaneLayout) -> Vec<Rect> {
        [layout.stories, layout.thread, layout.detail]
            .into_iter()
            .flatten()
            .collect()
    }

    fn assert_tiles(area: Rect, layout: PaneLayout) {
        let panes = visible(layout);
        assert!(!panes.is_empty());
        assert_eq!(panes[0].x, area.x);
        assert_eq!(panes.last().expect("pane exists").right(), area.right());
        assert!(
            panes
                .iter()
                .all(|pane| pane.y == area.y && pane.height == area.height)
        );
        for pair in panes.windows(2) {
            assert_eq!(pair[0].right(), pair[1].x, "pane gap or overlap");
        }
        assert_eq!(panes.iter().map(|pane| pane.width).sum::<u16>(), area.width);
    }

    #[test]
    fn defaults_are_valid_and_stable() {
        let preferences = LayoutPreferences::default()
            .validate()
            .expect("defaults validate");
        assert_eq!(preferences.mode, PaneMode::Two);
        assert_eq!(preferences.two, [44, 56]);
        assert_eq!(preferences.three, [38, 34, 28]);
        assert_eq!(preferences.two_min_width, 80);
        assert_eq!(preferences.three_min_width, 120);
    }

    #[test]
    fn boundary_widths_follow_requested_mode() {
        let area = |width| Rect::new(0, 0, width, 20);
        let two = LayoutPreferences::default();
        assert_eq!(
            resolve_panes(area(79), &two, FocusPane::Thread, SecondaryPane::Thread).mode,
            ResolvedMode::One
        );
        assert_eq!(
            resolve_panes(area(80), &two, FocusPane::Thread, SecondaryPane::Thread).mode,
            ResolvedMode::Two
        );

        let three = two.with_mode(PaneMode::Three);
        assert_eq!(
            resolve_panes(area(119), &three, FocusPane::Thread, SecondaryPane::Thread).mode,
            ResolvedMode::Two
        );
        assert_eq!(
            resolve_panes(area(120), &three, FocusPane::Thread, SecondaryPane::Thread).mode,
            ResolvedMode::Three
        );
    }

    #[test]
    fn layouts_tile_odd_widths_and_nonzero_origins() {
        for (width, mode) in [
            (121, PaneMode::Three),
            (81, PaneMode::Two),
            (17, PaneMode::Three),
        ] {
            let area = Rect::new(7, 11, width, 23);
            let preferences = LayoutPreferences::default().with_mode(mode);
            let layout =
                resolve_panes(area, &preferences, FocusPane::Detail, SecondaryPane::Detail);
            assert_tiles(area, layout);
        }
    }

    #[test]
    fn custom_breakpoints_are_honored() {
        let preferences = LayoutPreferences {
            mode: PaneMode::Three,
            two_min_width: 60,
            three_min_width: 90,
            ..LayoutPreferences::default()
        };
        assert_eq!(
            resolve_panes(
                Rect::new(0, 0, 89, 10),
                &preferences,
                FocusPane::Stories,
                SecondaryPane::Thread
            )
            .mode,
            ResolvedMode::Two
        );
        assert_eq!(
            resolve_panes(
                Rect::new(0, 0, 90, 10),
                &preferences,
                FocusPane::Stories,
                SecondaryPane::Thread
            )
            .mode,
            ResolvedMode::Three
        );
    }

    #[test]
    fn minimum_columns_force_further_fallback() {
        let preferences = LayoutPreferences {
            mode: PaneMode::Three,
            two: [85, 15],
            three: [70, 15, 15],
            two_min_width: 1,
            three_min_width: 1,
        };
        let layout = resolve_panes(
            Rect::new(0, 0, 100, 10),
            &preferences,
            FocusPane::Detail,
            SecondaryPane::Detail,
        );
        assert_eq!(layout.mode, ResolvedMode::One);
        assert_eq!(layout.detail.expect("detail visible").width, 100);
    }

    #[test]
    fn validation_rejects_bad_ratios_and_breakpoints() {
        assert!(matches!(
            LayoutPreferences {
                two: [50, 49],
                ..LayoutPreferences::default()
            }
            .validate(),
            Err(LayoutError::RatioTotal { .. })
        ));
        assert!(matches!(
            LayoutPreferences {
                three: [70, 20, 10],
                ..LayoutPreferences::default()
            }
            .validate(),
            Err(LayoutError::RatioTooSmall { .. })
        ));
        assert!(matches!(
            LayoutPreferences {
                two_min_width: 100,
                three_min_width: 90,
                ..LayoutPreferences::default()
            }
            .validate(),
            Err(LayoutError::BreakpointOrder { .. })
        ));
    }

    #[test]
    fn resizing_uses_the_focused_divider_and_enforces_minimums() {
        let two = LayoutPreferences::default();
        assert_eq!(
            two.resized(FocusPane::Stories, 2).expect("resize").two,
            [46, 54]
        );
        assert_eq!(
            two.resized(FocusPane::Thread, 2).expect("resize").two,
            [42, 58]
        );

        let three = two.with_mode(PaneMode::Three);
        assert_eq!(
            three.resized(FocusPane::Stories, 2).expect("resize").three,
            [40, 32, 28]
        );
        assert_eq!(
            three.resized(FocusPane::Thread, 2).expect("resize").three,
            [38, 36, 26]
        );
        assert_eq!(
            three.resized(FocusPane::Detail, 2).expect("resize").three,
            [38, 32, 30]
        );

        let extreme = LayoutPreferences {
            two: [85, 15],
            ..LayoutPreferences::default()
        };
        assert!(matches!(
            extreme.resized(FocusPane::Stories, 2),
            Err(LayoutError::ResizeRejected(15))
        ));
    }
}
