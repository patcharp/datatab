use super::layout;
use super::{SettingsSection, SettingsSectionId};
use datatab_app::keymap::Modifiers;
use datatab_components::primitives::Text;
use datatab_ui_base::keymap::key_chord_from_gpui;
use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;

/// Where "Report a bug" goes. The repository is private, so people who get an
/// installer reach the owner by mail instead.
const CONTACT_ADDRESS: &str = "me@pcplab.me";
const CONTACT_URL: &str = "mailto:me@pcplab.me";

pub(super) struct AboutSection {
    content_focused: bool,
}

impl AboutSection {
    pub(super) fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            content_focused: false,
        }
    }
}

/// The name before the address in the first entry of `CARGO_PKG_AUTHORS`.
fn author_display_name(authors: &str) -> &str {
    authors.split('<').next().unwrap_or(authors).trim()
}

impl SettingsSection for AboutSection {
    fn section_id(&self) -> SettingsSectionId {
        SettingsSectionId::About
    }

    /// The page has one link, so Enter or Space opens it and the other keys
    /// have nowhere to move the cursor.
    fn handle_key_event(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.content_focused {
            return;
        }

        let chord = key_chord_from_gpui(&event.keystroke);

        if matches!(chord.key.as_str(), "enter" | "space") && chord.modifiers == Modifiers::none() {
            cx.open_url(CONTACT_URL);
        }
    }

    fn focus_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = true;
        cx.notify();
    }

    fn focus_out(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = false;
        cx.notify();
    }
}

impl Render for AboutSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        const VERSION: &str = env!("CARGO_PKG_VERSION");
        const AUTHORS: &str = env!("CARGO_PKG_AUTHORS");
        const LICENSE: &str = env!("CARGO_PKG_LICENSE");

        #[cfg(debug_assertions)]
        const PROFILE: &str = "debug";
        #[cfg(not(debug_assertions))]
        const PROFILE: &str = "release";

        // Rendered through `img` (full color) rather than the monochrome icon
        // path so the channel-specific mark — including nightly — shows in color.
        let mark_path = match datatab_core::ReleaseChannel::current() {
            datatab_core::ReleaseChannel::Nightly => "branding/nightly/mark-256.png",
            _ => "branding/stable/mark-256.png",
        };

        let contact_focused = self.content_focused;
        let author_name = author_display_name(AUTHORS);
        let license_display = LICENSE.replace(" OR ", " and ");
        let copyright_line = crate::labels::about_copyright(author_name);
        let license_line = crate::labels::about_license(&license_display);
        let derived_notice = datatab_i18n::t!("settings.about.derived_notice");

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(datatab_components::composites::page_header(
                datatab_i18n::t!("settings.about.title"),
                datatab_i18n::t!("settings.about.subtitle"),
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px(crate::tokens::SettingsMetrics::BODY_PADDING_X)
                    .py(datatab_components::tokens::Spacing::LG)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(img(mark_path).size(px(65.0)))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(Text::heading("DataTab"))
                                            .child(
                                                Text::code(format!("{} ({})", VERSION, PROFILE))
                                                    .muted_foreground(),
                                            ),
                                    ),
                            )
                            .child(
                                // items_baseline + Body-wrapped fillers keeps
                                // the link and the address beside it on the
                                // same baseline; bare &str children sit on a
                                // different metric and look pulled up.
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_1()
                                    .child(layout::cursor_ring(
                                        contact_focused,
                                        div()
                                            .id("about-link-contact")
                                            .cursor_pointer()
                                            .hover(|d| d.underline())
                                            .on_click(|_, _, cx| {
                                                cx.open_url(CONTACT_URL);
                                            })
                                            .child(
                                                Text::body(datatab_i18n::t!(
                                                    "settings.about.report_bug"
                                                ))
                                                .color(theme.link),
                                            ),
                                        cx,
                                    ))
                                    // The platform gives no answer when no mail
                                    // client opens, so the address stays on the
                                    // page for the user to read and type.
                                    .child(
                                        Text::body(format!("({CONTACT_ADDRESS})"))
                                            .color(theme.muted_foreground),
                                    ),
                            )
                            .child(Text::body(copyright_line))
                            .child(Text::body(license_line))
                            .child(
                                div()
                                    .id("about-derived-notice")
                                    .role(Role::Note)
                                    .aria_label(derived_notice.clone())
                                    .child(Text::body(derived_notice)),
                            )
                            .child(
                                div()
                                    .mt_4()
                                    .pt_4()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(Text::body(datatab_i18n::t!(
                                        "settings.about.third_party_licenses"
                                    )))
                                    .child(
                                        Text::body(datatab_i18n::t!("settings.about.lucide"))
                                            .color(theme.muted_foreground),
                                    )
                                    .child(
                                        Text::body(datatab_i18n::t!("settings.about.simple_icons"))
                                            .color(theme.muted_foreground),
                                    ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{CONTACT_ADDRESS, CONTACT_URL, author_display_name};
    use crate::labels::{about_copyright, about_license};

    const ABOUT_CATALOG_KEYS: &[&str] = &[
        "settings.about.title",
        "settings.about.subtitle",
        "settings.about.report_bug",
        "settings.about.copyright",
        "settings.about.license",
        "settings.about.derived_notice",
        "settings.about.third_party_licenses",
        "settings.about.lucide",
        "settings.about.simple_icons",
    ];

    #[test]
    fn settings_about_keys_resolve() {
        for key in ABOUT_CATALOG_KEYS {
            let value = datatab_i18n::t!(key, locale = "en");

            assert!(!value.is_empty(), "key {key} resolved empty");
            assert_ne!(value, *key, "key {key} did not resolve");
            assert_ne!(
                value,
                format!("en.{key}"),
                "key {key} fell back to the raw en-qualified form"
            );
        }
    }

    #[test]
    fn settings_about_title_english_text() {
        let english = datatab_i18n::t!("settings.about.title", locale = "en");

        assert_eq!(english, "About");
    }

    #[test]
    fn about_copyright_embeds_author_name() {
        let en = about_copyright("Jane Doe");

        assert!(en.contains("Jane Doe"));
    }

    #[test]
    fn about_license_embeds_license_identifier() {
        let en = about_license("MIT and Apache-2.0");

        assert!(en.contains("MIT and Apache-2.0"));
    }

    #[test]
    fn the_copyright_line_names_pcplab() {
        let author = author_display_name(env!("CARGO_PKG_AUTHORS"));

        assert_eq!(author, "PCPLab");

        let line = datatab_i18n::translate_in("en", "settings.about.copyright")
            .replace("%{author}", author);

        assert!(line.contains("PCPLab"), "{line}");
        assert!(!line.contains("%{"), "{line}");
    }

    #[test]
    fn the_contact_link_is_a_mail_to_the_shown_address() {
        assert_eq!(CONTACT_URL, format!("mailto:{CONTACT_ADDRESS}"));
    }
}
