//! `LoadingState<T>` — generic primitive for representing async fetch state,
//! and `Spinner`, the cell-shaped loading indicator.

use gpui::prelude::*;
use gpui::{
    App, Bounds, ElementId, IntoElement, Pixels, Role, SharedString, Window, canvas, div, fill,
    point, relative, size,
};

use gpui_component::ActiveTheme;

use crate::selector::id_selector;
use crate::tokens::{ChromeColors, Feedback, Radii};

/// Generic async-fetch state. The caller renders every phase.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadingState<T> {
    /// Fetch not yet started.
    Idle,
    /// Fetch in progress.
    Loading,
    /// Fetch resolved successfully.
    Loaded(T),
    /// Fetch failed with an error message.
    Failed { message: SharedString },
}

impl<T> LoadingState<T> {
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    pub fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded(_))
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// Map the loaded value without consuming self.
    pub fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(v) => Some(v),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

/// Six cells form the track, in the order the active cell walks around it.
const SPINNER_CELLS: [(f32, f32); 6] = [
    (0.0, 0.0),
    (1.0, 0.0),
    (2.0, 1.0),
    (1.0, 2.0),
    (0.0, 2.0),
    (0.0, 1.0),
];
const SPINNER_FRAMES: usize = SPINNER_CELLS.len();

/// Six-cell loading indicator. Callers advance `frame` every
/// [`Spinner::INTERVAL_MS`] ms and notify the view. Reduced motion shows
/// all cells in the full tint without animation.
#[derive(IntoElement)]
pub struct Spinner {
    frame: usize,
}

impl Spinner {
    /// Create a spinner at the given animation frame.
    pub fn new(frame: usize) -> Self {
        Self {
            frame: frame % SPINNER_FRAMES,
        }
    }

    /// Advance frame mod the frame count — call this every `INTERVAL_MS` ms.
    pub fn next_frame(frame: usize) -> usize {
        (frame + 1) % SPINNER_FRAMES
    }

    pub const INTERVAL_MS: u64 = Feedback::SPINNER_STEP_MS;
}

impl RenderOnce for Spinner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tint = ChromeColors::tint(cx.theme());
        let track = tint.opacity(Feedback::SPINNER_TRACK_ALPHA);
        let active = active_cell(self.frame, cx.reduce_motion());

        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(Feedback::SPINNER_BOX)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        for (index, cell) in spinner_cell_bounds(bounds).into_iter().enumerate() {
                            let color = if active.is_none() || active == Some(index) {
                                tint
                            } else {
                                track
                            };
                            window.paint_quad(fill(cell, color));
                        }
                    },
                )
                .size(Feedback::SPINNER_CELLS_SIZE),
            )
    }
}

fn active_cell(frame: usize, reduce_motion: bool) -> Option<usize> {
    if reduce_motion {
        None
    } else {
        Some(frame % SPINNER_FRAMES)
    }
}

/// Scale the six square cells and their gaps into the spinner bounds.
fn spinner_cell_bounds(bounds: Bounds<Pixels>) -> [Bounds<Pixels>; SPINNER_FRAMES] {
    let side = bounds.size.width.min(bounds.size.height);
    let cell = side * (4.0 / 14.0);
    let step = side * (5.0 / 14.0);
    SPINNER_CELLS.map(|(column, row)| {
        Bounds::new(
            point(
                bounds.origin.x + step * column,
                bounds.origin.y + step * row,
            ),
            size(cell, cell),
        )
    })
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// Determinate progress bar: a thin track that fills from left to right as a
/// value moves through `0.0..=1.0`.
///
/// The bar clamps the value it is given and announces it to assistive
/// technology as a progress indicator. It draws the same two tint roles as
/// the [`Spinner`]: the full tint for the fill and the tint at a faint
/// alpha for the track. Callers size the bar's surroundings; the bar spans
/// the width its parent gives it.
#[derive(IntoElement)]
pub struct Progress {
    id: SharedString,
    value: f32,
    label: Option<SharedString>,
}

impl Progress {
    /// Create a bar for `value`, clamped into `0.0..=1.0`, under the
    /// caller's element id. A non-finite value (`NaN`, an infinity) has no
    /// place on a bar and reads as `0.0`.
    pub fn new(id: impl Into<SharedString>, value: f32) -> Self {
        Self {
            id: id.into(),
            value: if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.0
            },
            label: None,
        }
    }

    /// Names the bar for assistive technology, e.g. "3 of 12 requests".
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Element id of the fill of the bar whose track is `id`.
pub fn progress_fill_id(id: &str) -> SharedString {
    SharedString::from(format!("{id}-fill"))
}

impl RenderOnce for Progress {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tint = ChromeColors::tint(&theme);
        let track = tint.opacity(Feedback::PROGRESS_TRACK_ALPHA);
        let fill_id = progress_fill_id(&self.id);
        let bar_id = ElementId::from(self.id.clone());

        div()
            .debug_selector(id_selector(&bar_id))
            .id(bar_id)
            .role(Role::ProgressIndicator)
            .when_some(self.label, |bar, label| bar.aria_label(label))
            .aria_min_numeric_value(0.0)
            .aria_max_numeric_value(1.0)
            .aria_numeric_value(f64::from(self.value))
            .flex()
            .w_full()
            .h(Feedback::PROGRESS_TRACK_HEIGHT)
            .rounded(Radii::SM)
            .bg(track)
            .child(
                div()
                    .debug_selector(id_selector(&fill_id.clone().into()))
                    .id(fill_id)
                    .h_full()
                    .w(relative(self.value))
                    .rounded(Radii::SM)
                    .bg(tint),
            )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::LoadingState;

    #[test]
    fn loading_state_transitions_are_exclusive() {
        let idle: LoadingState<i32> = LoadingState::Idle;
        assert!(idle.is_idle());
        assert!(!idle.is_loading());
        assert!(!idle.is_loaded());
        assert!(!idle.is_failed());

        let loading: LoadingState<i32> = LoadingState::Loading;
        assert!(loading.is_loading());
        assert!(!loading.is_idle());

        let loaded = LoadingState::Loaded(42i32);
        assert!(loaded.is_loaded());
        assert_eq!(loaded.loaded(), Some(&42i32));

        let failed: LoadingState<i32> = LoadingState::Failed {
            message: "oops".into(),
        };
        assert!(failed.is_failed());
        assert_eq!(failed.loaded(), None);
    }

    #[test]
    fn spinner_step_and_cycle_match_design() {
        assert_eq!(super::Spinner::INTERVAL_MS, 200);
        assert_eq!(
            super::Spinner::INTERVAL_MS * super::SPINNER_FRAMES as u64,
            1200
        );
        assert_eq!(super::Feedback::SPINNER_CELLS_SIZE, gpui::px(14.0));
    }

    #[test]
    fn spinner_next_frame_wraps_around() {
        use super::{SPINNER_FRAMES, Spinner};

        assert_eq!(Spinner::next_frame(0), 1);
        assert_eq!(Spinner::next_frame(SPINNER_FRAMES - 2), SPINNER_FRAMES - 1);
        assert_eq!(Spinner::next_frame(SPINNER_FRAMES - 1), 0);
    }

    #[test]
    fn spinner_walks_six_square_cells() {
        use super::{SPINNER_FRAMES, Spinner, spinner_cell_bounds};
        use gpui::{Bounds, point, px, size};

        let cells = spinner_cell_bounds(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(14.0), px(14.0)),
        ));
        let positions: Vec<_> = cells
            .iter()
            .map(|cell| (f32::from(cell.origin.x), f32::from(cell.origin.y)))
            .collect();
        assert_eq!(
            positions,
            [
                (0.0, 0.0),
                (5.0, 0.0),
                (10.0, 5.0),
                (5.0, 10.0),
                (0.0, 10.0),
                (0.0, 5.0)
            ]
        );
        assert!(cells.iter().all(|cell| {
            f32::from(cell.size.width) == 4.0 && f32::from(cell.size.height) == 4.0
        }));
        assert_eq!(Spinner::new(SPINNER_FRAMES + 3).frame, 3);
        assert_eq!(super::active_cell(3, false), Some(3));
        assert_eq!(super::active_cell(3, true), None);
    }

    #[test]
    fn spinner_renderer_paints_the_tested_cells() {
        let source = include_str!("loading_state.rs");
        let production = source.split("// Progress").next().expect("spinner section");
        assert!(production.contains("spinner_cell_bounds(bounds).into_iter()"));
        assert!(production.contains("window.paint_quad(fill(cell, color))"));
        assert!(!production.contains("PathBuilder"));
    }

    // --- Progress ------------------------------------------------------------

    use gpui::{
        AccessibilityFrame, AnyElement, App, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, accesskit, px,
    };

    use crate::accessibility::{FrameCapture, accessibility_node, accessible_name};

    /// Renders `build` alone in a themed test window whose surface is a
    /// fixed 300 px wide, so the fill fraction can be read off the laid-out
    /// bounds. Returns the announced frame and the window.
    fn progress_window(
        cx: &mut TestAppContext,
        build: impl Fn(&mut App) -> AnyElement + 'static,
    ) -> (AccessibilityFrame, &mut VisualTestContext) {
        struct Surface(Box<dyn Fn(&mut App) -> AnyElement>);

        impl Render for Surface {
            fn render(
                &mut self,
                _window: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                gpui::div()
                    .debug_selector(|| "progress-under-test".to_string())
                    .w(px(300.0))
                    .child((self.0)(cx))
            }
        }

        cx.update(gpui_component::init);
        cx.update(|cx| {
            crate::theme::install(
                crate::brand::Brand::neutral(),
                crate::palette::PaletteMode::Dark,
                crate::density::Density::Default,
                cx,
            );
        });

        let (_, window) = cx.add_window_view(move |_, _| Surface(Box::new(build)));
        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);
        (frame, window)
    }

    /// The fill width follows the value: half fills half the track, full
    /// fills all of it, both from the track's left edge.
    #[gpui::test]
    fn the_fill_follows_the_value(cx: &mut TestAppContext) {
        let (_, window) = progress_window(cx, |_cx| {
            super::Progress::new("run", 0.5).into_any_element()
        });

        // `debug_bounds` needs a 'static id; the derivation helper is pinned
        // by `progress_fill_id_derives_the_fill_id` below.
        let track = window.debug_bounds("run").expect("the track is laid out");
        let fill = window
            .debug_bounds("run-fill")
            .expect("the fill is laid out");

        assert_eq!(
            fill.left(),
            track.left(),
            "the fill starts at the track's left edge"
        );
        let half = f32::from(track.size.width) / 2.0;
        assert!(
            (f32::from(fill.size.width) - half).abs() < 0.5,
            "half fills half the track: fill {} of track {}",
            fill.size.width,
            track.size.width
        );

        let (_, window) = progress_window(cx, |_cx| {
            super::Progress::new("run", 1.0).into_any_element()
        });

        let track = window.debug_bounds("run").expect("the track is laid out");
        let fill = window
            .debug_bounds("run-fill")
            .expect("the fill is laid out");
        assert!(
            (f32::from(fill.size.width) - f32::from(track.size.width)).abs() < 0.5,
            "full fills the whole track: fill {} of track {}",
            fill.size.width,
            track.size.width
        );
    }

    /// The id the fill renders under: the track's id with a `-fill` suffix,
    /// the same derivation the render and the tests read.
    #[test]
    fn progress_fill_id_derives_the_fill_id() {
        assert_eq!(super::progress_fill_id("run").as_ref(), "run-fill");
    }

    /// A value outside `0.0..=1.0` clamps: the bar announces the clamped
    /// value between its announced bounds.
    #[gpui::test]
    fn the_value_is_clamped_into_the_unit_range(cx: &mut TestAppContext) {
        let (frame, _) = progress_window(cx, |_cx| {
            super::Progress::new("run", 1.5).into_any_element()
        });

        let node = accessibility_node(&frame, "run").expect("the bar is announced");
        assert_eq!(node.numeric_value(), Some(1.0));
        assert_eq!(node.min_numeric_value(), Some(0.0));
        assert_eq!(node.max_numeric_value(), Some(1.0));

        let (frame, _) = progress_window(cx, |_cx| {
            super::Progress::new("run", -0.25).into_any_element()
        });

        let node = accessibility_node(&frame, "run").expect("the bar is announced");
        assert_eq!(node.numeric_value(), Some(0.0));
    }

    /// A non-finite value reads as zero: the bar has nothing to announce
    /// for a NaN or an infinite progress.
    #[gpui::test]
    fn a_non_finite_value_reads_as_zero(cx: &mut TestAppContext) {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let (frame, _) = progress_window(cx, move |_cx| {
                super::Progress::new("run", value).into_any_element()
            });

            let node = accessibility_node(&frame, "run").expect("the bar is announced");
            assert_eq!(
                node.numeric_value(),
                Some(0.0),
                "a non-finite value ({value}) reads as zero"
            );
        }
    }

    /// The bar announces itself under the caller's id with the caller's
    /// label, and its fill sits under the bar's own derived id.
    #[gpui::test]
    fn the_bar_carries_the_callers_id_and_label(cx: &mut TestAppContext) {
        let (frame, _) = progress_window(cx, |_cx| {
            super::Progress::new("run", 0.25)
                .label("3 of 12 requests")
                .into_any_element()
        });

        assert_eq!(
            accessible_name(&frame, "run").as_deref(),
            Some("3 of 12 requests"),
            "the caller's label names the bar"
        );
        let node = accessibility_node(&frame, "run").expect("the bar is announced");
        assert_eq!(node.role(), accesskit::Role::ProgressIndicator);
        assert_eq!(node.numeric_value(), Some(0.25));
        let fill_id = super::progress_fill_id("run");
        assert!(
            frame.nodes().any(|(_, node)| node.id() == fill_id.as_ref()),
            "the fill renders under the bar's derived id"
        );

        let (frame, _) = progress_window(cx, |_cx| {
            super::Progress::new("run", 0.25).into_any_element()
        });

        assert_eq!(
            accessible_name(&frame, "run"),
            None,
            "a bar without a label carries no name of its own"
        );
    }
}
