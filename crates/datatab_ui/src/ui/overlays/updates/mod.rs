//! The "What is new" dialog and the first-run welcome screen.
//!
//! Both are `modals::Modal` cards drawn with the card cut (14) the boards
//! use. "What is new" renders the bundled changelog and writes the update
//! preference straight to storage when its checkbox changes, so closing the
//! dialog never loses a choice. The welcome screen teaches three keys and
//! offers to add a connection; it shows no changelog. The changelog list
//! lives here.

pub mod welcome;
pub mod whats_new;

pub use welcome::{WelcomeDialog, WelcomeEvent};
pub use whats_new::WhatsNewDialog;

use datatab_app::updates::{self, ChangelogRelease, ChangelogSection, ReleaseHeading, SectionKind};
use datatab_components::icons::AppIcon;
use datatab_components::primitives::{Icon, Text};
use datatab_components::tokens::{ChromeColors, FontSizes, Spacing, ui};
use datatab_ui_base::AppStateEntity;
use datatab_ui_base::user_error::{ErrorKind, UserFacingError, report_error};
use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::scroll::Scrollbar;

/// Size of the icon in front of each changelog entry and column heading.
const ENTRY_ICON_SIZE: Rems = ui(15.0);

/// Size of the icon in front of a changelog section label.
const SECTION_ICON_SIZE: Rems = FontSizes::XS;

/// Gap between a section icon and its label.
const SECTION_HEADING_GAP: Pixels = px(7.0);

/// Gap between an entry icon and its text, and between a checkbox and its
/// label.
const ROW_GAP: Pixels = px(10.0);

/// Gap between an entry title and its summary.
const ENTRY_TEXT_GAP: Pixels = px(2.0);

/// Gap between a checkbox label and its helper text.
const HINT_GAP: Pixels = px(3.0);

/// Vertical padding of a checkbox row.
const PREFERENCE_PADDING_Y: Pixels = Spacing::XXS;

/// Persists the "show what is new after an update" preference, reporting a
/// storage failure to the user who just toggled it.
pub(crate) fn set_show_whats_new(app_state: &Entity<AppStateEntity>, value: bool, cx: &mut App) {
    let result = app_state.update(cx, |state, cx| {
        let settings = updates::UpdateSettings {
            show_whats_new_after_update: value,
            ..state.update_settings().clone()
        };

        let result = state.set_update_settings(settings);
        cx.emit(datatab_ui_base::AppStateChanged);
        cx.notify();
        result
    });

    if let Err(error) = result {
        report_error(
            UserFacingError::new(
                ErrorKind::Storage,
                datatab_i18n::t!("updates.settings.save_error", error = error),
            ),
            cx,
        );
    }
}

fn section_label(kind: &SectionKind) -> String {
    match kind {
        SectionKind::Added => datatab_i18n::t!("updates.sections.added"),
        SectionKind::Changed => datatab_i18n::t!("updates.sections.changed"),
        SectionKind::Deprecated => datatab_i18n::t!("updates.sections.deprecated"),
        SectionKind::Removed => datatab_i18n::t!("updates.sections.removed"),
        SectionKind::Fixed => datatab_i18n::t!("updates.sections.fixed"),
        SectionKind::Security => datatab_i18n::t!("updates.sections.security"),
        SectionKind::Other(heading) => heading.clone(),
    }
}

fn section_icon(kind: &SectionKind, theme: &gpui_component::Theme) -> (AppIcon, Hsla) {
    match kind {
        SectionKind::Added => (AppIcon::Plus, ChromeColors::tint(theme)),
        SectionKind::Changed => (AppIcon::Pencil, ChromeColors::tint(theme)),
        SectionKind::Fixed => (AppIcon::Check, theme.success),
        SectionKind::Removed => (AppIcon::Minus, theme.danger),
        SectionKind::Deprecated | SectionKind::Security => (AppIcon::TriangleAlert, theme.warning),
        SectionKind::Other(_) => (AppIcon::ChevronRight, theme.muted_foreground),
    }
}

/// The name a release is shown under: its version, or "Unreleased".
pub(crate) fn release_label(release: &ChangelogRelease) -> String {
    match &release.heading {
        ReleaseHeading::Version(version) => version.to_string(),
        ReleaseHeading::Unreleased => datatab_i18n::t!("updates.whats_new.unreleased"),
    }
}

/// `0.8.0  2026-10-15`: the release version in strong mono and its date.
pub(crate) fn release_header(release: &ChangelogRelease, cx: &App) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(ROW_GAP)
        .child(
            Text::code(release_label(release))
                .font_size(FontSizes::SM)
                .font_weight(FontWeight::BOLD)
                .text_color(ChromeColors::strong(cx.theme())),
        )
        .when_some(release.date.clone(), |row, date| {
            row.child(Text::caption(date))
        })
}

/// A changelog section: its labeled heading and up to `limit` entries, each
/// padded by `entry_padding_y` above and below.
///
/// An entry is the section icon, its title and one muted summary line that
/// ends in an ellipsis when it does not fit; the title wraps.
pub(crate) fn render_section(
    section: &ChangelogSection,
    limit: usize,
    entry_padding_y: Pixels,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let (icon, icon_color) = section_icon(&section.kind, theme);
    let strong = ChromeColors::strong(theme);
    let muted = theme.muted_foreground;

    let heading = div()
        .flex()
        .items_center()
        .gap(SECTION_HEADING_GAP)
        .pt(Spacing::XXS)
        .child(Icon::new(icon).size(SECTION_ICON_SIZE).color(icon_color))
        .child(Text::label(section_label(&section.kind)));

    let entries = section.entries.iter().take(limit).map(|entry| {
        div()
            .flex()
            .items_start()
            .gap(ROW_GAP)
            .py(entry_padding_y)
            .child(Icon::new(icon).size(ENTRY_ICON_SIZE).color(muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ENTRY_TEXT_GAP)
                    .child(Text::body(entry.title.clone()).text_color(strong))
                    .when_some(entry.summary.clone(), |column, summary| {
                        column.child(div().min_w_0().truncate().child(Text::caption(summary)))
                    }),
            )
    });

    div().flex().flex_col().child(heading).children(entries)
}

/// A vertically scrolling region that `handle` tracks, with the themed
/// scrollbar over its right edge.
///
/// The region takes its content's height and shrinks from there, so the
/// caller only caps it: a `max_h` on the parent, or `min_h_0` inside a
/// column of fixed height. Heights stay content-based on purpose: a
/// percentage height inside an auto-sized parent, such as the modal body,
/// resolves to nothing and collapses the region.
pub(crate) fn scroll_region(
    id: &'static str,
    handle: &ScrollHandle,
    content: impl IntoElement,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .child(
            div()
                .id(id)
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(handle)
                .child(div().flex_none().child(content)),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .child(Scrollbar::vertical(handle)),
        )
}

/// A checkbox row with a label and optional helper text below it. The text
/// wraps within the row.
pub(crate) fn preference_row(
    id: &'static str,
    label: String,
    hint: Option<String>,
    checked: bool,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .items_start()
        .gap(ROW_GAP)
        .py(PREFERENCE_PADDING_Y)
        .child(
            datatab_components::controls::Checkbox::new(id)
                .checked(checked)
                .aria_label(label.clone())
                .on_click(move |value: &bool, window, cx| on_toggle(*value, window, cx)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(HINT_GAP)
                .child(Text::body(label))
                .when_some(hint, |column, hint| column.child(Text::caption(hint))),
        )
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::app::AppStateEntity;
    use datatab_storage::bootstrap::StorageRuntime;
    use gpui::{
        AppContext as _, Entity, Modifiers, Point, ScrollDelta, ScrollHandle, ScrollWheelEvent,
        TestAppContext, VisualTestContext, point, px,
    };

    /// A changelog with releases 0.8.0, 0.7.0 and 0.6.0 whose sections are
    /// long enough to overflow a dialog, so the dialog tests do not depend on
    /// the notes this build ships.
    pub(crate) fn fixture_releases() -> &'static [datatab_app::updates::ChangelogRelease] {
        static RELEASES: std::sync::OnceLock<Vec<datatab_app::updates::ChangelogRelease>> =
            std::sync::OnceLock::new();

        RELEASES.get_or_init(|| {
            let mut markdown = String::from("# Changelog\n\n");
            for (version, date) in [
                ("0.8.0", "2026-10-15"),
                ("0.7.0", "2026-07-31"),
                ("0.6.0", "2026-05-14"),
            ] {
                markdown.push_str(&format!("## [{version}] - {date}\n\n"));
                for section in ["Added", "Changed", "Fixed"] {
                    markdown.push_str(&format!("### {section}\n\n"));
                    for number in 1..=8 {
                        markdown.push_str(&format!(
                            "* **{section} {number} of {version}** — A sentence that explains the change in some detail.\n"
                        ));
                    }
                    markdown.push('\n');
                }
            }

            datatab_app::updates::changelog::parse(&markdown)
        })
        .as_slice()
    }

    pub(crate) fn test_app_state(cx: &mut TestAppContext) -> Entity<AppStateEntity> {
        cx.update(datatab_components::theme::init);
        cx.update(datatab_ui_base::keymap::init_keymap);

        cx.update(|cx| {
            cx.new(|_| {
                AppStateEntity::new_with_storage_runtime(
                    StorageRuntime::in_memory().expect("test storage runtime"),
                )
                .expect("test app state")
            })
        })
    }

    /// Asserts that the region `handle` tracks overflows, then scrolls with
    /// the mouse wheel over it and with the navigation keys.
    pub(crate) fn assert_scrolls_by_wheel_and_keys(
        handle: &ScrollHandle,
        window: &mut VisualTestContext,
    ) {
        let bounds = handle.bounds();
        let max = handle.max_offset().y;
        assert!(max > px(0.0), "the list does not overflow: {bounds:?}");
        assert_eq!(handle.offset().y, px(0.0));

        window.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-60.0))),
            modifiers: Modifiers::default(),
            ..Default::default()
        });
        window.run_until_parked();
        assert!(
            handle.offset().y < px(0.0),
            "the wheel did not scroll the list"
        );

        window.simulate_keystrokes("home");
        window.run_until_parked();
        assert_eq!(handle.offset(), Point::default());

        window.simulate_keystrokes("down");
        window.run_until_parked();
        let after_line = handle.offset().y;
        assert!(
            after_line < px(0.0),
            "the down arrow did not scroll the list"
        );

        window.simulate_keystrokes("pagedown");
        window.run_until_parked();
        assert!(handle.offset().y < after_line || handle.offset().y == -max);

        window.simulate_keystrokes("end");
        window.run_until_parked();
        assert_eq!(handle.offset().y, -max);

        window.simulate_keystrokes("pageup");
        window.run_until_parked();
        assert!(handle.offset().y > -max);
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent's `gpui::*` glob would make `#[test]`
    // resolve to `gpui::test`, whose expansion recurses without bound.
    use super::{SectionKind, section_label};

    #[test]
    fn section_labels_resolve() {
        for key in [
            "updates.sections.added",
            "updates.sections.changed",
            "updates.sections.deprecated",
            "updates.sections.removed",
            "updates.sections.fixed",
            "updates.sections.security",
        ] {
            let value = datatab_i18n::t!(key, locale = "en");
            assert_ne!(value, format!("en.{key}"), "{key} missing");
        }

        assert_eq!(
            section_label(&SectionKind::Other("Chores".to_string())),
            "Chores"
        );
    }
}
