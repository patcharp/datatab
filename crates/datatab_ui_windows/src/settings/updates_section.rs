use super::section_trait::SectionFocusEvent;
use super::{SettingsSection, SettingsSectionId};
use datatab_app::keymap::Modifiers;
use datatab_app::updates::UpdateSettings;
use datatab_components::controls::{Button, Checkbox};
use datatab_components::icons::AppIcon;
use datatab_components::primitives::{Icon, Text};
use datatab_components::tokens::{FontSizes, Spacing, ui};
use datatab_core::ReleaseChannel;
use datatab_ui_base::keymap::key_chord_from_gpui;
use datatab_ui_base::toast::{Toast, now_hms};
use datatab_ui_base::updates::{UpdateDialogRequest, channel_label, running_version_label};
use datatab_ui_base::user_error::{ErrorKind, UserFacingError, report_error};
use datatab_ui_base::{AppStateChanged, AppStateEntity};
use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;

/// Width of the label column in a settings row.
const ROW_LABEL_WIDTH: Rems = ui(200.0);

/// Vertical padding of a settings row.
const ROW_PADDING_Y: Pixels = px(7.0);

/// Keyboard-navigable controls of the section, in visual order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdatesRow {
    ShowWhatsNew,
    OpenWhatsNew,
    ShowWelcome,
    Reset,
    Save,
}

const ROWS: [UpdatesRow; 5] = [
    UpdatesRow::ShowWhatsNew,
    UpdatesRow::OpenWhatsNew,
    UpdatesRow::ShowWelcome,
    UpdatesRow::Reset,
    UpdatesRow::Save,
];

/// Settings › Updates: the changelog preference, the running version, and
/// entry points to the two dialogs.
pub(super) struct UpdatesSection {
    app_state: Entity<AppStateEntity>,
    show_whats_new: bool,
    content_focused: bool,
    cursor: usize,
    _subscriptions: Vec<Subscription>,
}

impl UpdatesSection {
    pub(super) fn new(
        app_state: Entity<AppStateEntity>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let saved = app_state.read(cx).update_settings().clone();
        let subscription = cx.subscribe(&app_state, |_this, _, _: &AppStateChanged, cx| {
            cx.notify();
        });

        Self {
            app_state,
            show_whats_new: saved.show_whats_new_after_update,
            content_focused: false,
            cursor: 0,
            _subscriptions: vec![subscription],
        }
    }

    fn saved_settings(&self, cx: &App) -> UpdateSettings {
        self.app_state.read(cx).update_settings().clone()
    }

    fn has_unsaved_changes(&self, cx: &App) -> bool {
        let saved = self.saved_settings(cx);

        saved.show_whats_new_after_update != self.show_whats_new
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let settings = UpdateSettings {
            show_whats_new_after_update: self.show_whats_new,
            ..self.saved_settings(cx)
        };

        let result = self.app_state.update(cx, |state, cx| {
            let result = state.set_update_settings(settings);
            cx.emit(AppStateChanged);
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
            return;
        }

        Toast::success(datatab_i18n::t!("updates.settings.saved"))
            .meta_right(now_hms())
            .push(cx);
        cx.notify();
    }

    fn reset_to_defaults(&mut self, cx: &mut Context<Self>) {
        let defaults = UpdateSettings::default();
        self.show_whats_new = defaults.show_whats_new_after_update;
        cx.notify();
    }

    fn request_dialog(&self, request: UpdateDialogRequest, cx: &mut Context<Self>) {
        self.app_state.update(cx, |state, cx| {
            state.request_update_dialog(request, cx);
        });
    }

    fn activate(&mut self, row: UpdatesRow, cx: &mut Context<Self>) {
        match row {
            UpdatesRow::ShowWhatsNew => self.show_whats_new = !self.show_whats_new,
            UpdatesRow::OpenWhatsNew => {
                self.request_dialog(UpdateDialogRequest::WhatsNew { since: None }, cx)
            }
            UpdatesRow::ShowWelcome => self.request_dialog(UpdateDialogRequest::Welcome, cx),
            UpdatesRow::Reset => self.reset_to_defaults(cx),
            UpdatesRow::Save => self.save(cx),
        }
        cx.notify();
    }

    fn select(&mut self, row: UpdatesRow) {
        self.content_focused = true;
        if let Some(position) = ROWS.iter().position(|candidate| *candidate == row) {
            self.cursor = position;
        }
    }

    fn is_at(&self, row: UpdatesRow) -> bool {
        self.content_focused && ROWS.get(self.cursor).copied() == Some(row)
    }

    fn focus_frame(&self, row: UpdatesRow, cx: &App, child: impl IntoElement) -> Div {
        super::layout::cursor_ring(self.is_at(row), child, cx)
    }

    fn render_group_header(&self, cx: &App) -> AnyElement {
        datatab_components::composites::section_header(
            datatab_i18n::t!("updates.settings.group"),
            Some(AppIcon::Download.into()),
            cx,
        )
        .into_any_element()
    }

    fn render_row(&self, label: String, content: impl IntoElement) -> AnyElement {
        div()
            .flex()
            .items_start()
            .gap(Spacing::LG)
            .py(ROW_PADDING_Y)
            .child(
                div()
                    .w(ROW_LABEL_WIDTH)
                    .flex_shrink_0()
                    .pt(ROW_PADDING_Y)
                    .child(Text::body(label)),
            )
            .child(div().flex_1().min_w_0().child(content))
            .into_any_element()
    }

    fn render_checkbox(
        &self,
        id: &'static str,
        row: UpdatesRow,
        label: String,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.focus_frame(
            row,
            cx,
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(Spacing::SM)
                .py(Spacing::XXS)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.select(row);
                        cx.notify();
                    }),
                )
                .child(
                    Checkbox::new(id)
                        .checked(checked)
                        .aria_label(label.clone())
                        .on_click(cx.listener(move |this, value: &bool, _, cx| {
                            this.select(row);
                            this.show_whats_new = *value;
                            cx.notify();
                        })),
                )
                .child(Text::body(label)),
        )
        .into_any_element()
    }

    fn render_button(
        &self,
        id: &'static str,
        row: UpdatesRow,
        label: String,
        icon: AppIcon,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Button::new(id, label)
            .secondary()
            .icon(icon)
            .focused(self.is_at(row))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(row);
                this.activate(row, cx);
            }))
            .into_any_element()
    }

    fn render_status(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();

        div()
            .flex()
            .items_center()
            .gap(Spacing::MD)
            .child(
                Icon::new(AppIcon::Info)
                    .size(ui(15.0))
                    .color(theme.muted_foreground),
            )
            .child(
                Text::code(status_detail())
                    .font_size(FontSizes::XS)
                    .text_color(theme.muted_foreground),
            )
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = self.render_status(cx);

        let changelog = div()
            .flex()
            .items_center()
            .gap(Spacing::SM)
            .child(self.render_checkbox(
                "updates-show-whats-new",
                UpdatesRow::ShowWhatsNew,
                datatab_i18n::t!("updates.settings.show_whats_new"),
                self.show_whats_new,
                cx,
            ))
            .child(div().flex_1())
            .child(self.render_button(
                "updates-open-whats-new",
                UpdatesRow::OpenWhatsNew,
                datatab_i18n::t!("updates.settings.open_whats_new"),
                AppIcon::History,
                cx,
            ))
            .child(self.render_button(
                "updates-show-welcome",
                UpdatesRow::ShowWelcome,
                datatab_i18n::t!("updates.settings.show_welcome"),
                AppIcon::Layers,
                cx,
            ));

        let group_header = self.render_group_header(cx);

        div()
            .flex()
            .flex_col()
            .child(group_header)
            .child(self.render_row(datatab_i18n::t!("updates.settings.status"), status))
            .child(self.render_row(datatab_i18n::t!("updates.settings.changelog"), changelog))
            .into_any_element()
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let chord = key_chord_from_gpui(&event.keystroke);

        match (chord.key.as_str(), chord.modifiers) {
            ("s", modifiers) if modifiers == Modifiers::ctrl() => {
                self.save(cx);
            }
            ("j", modifiers) | ("down", modifiers) | ("tab", modifiers)
                if modifiers == Modifiers::none() =>
            {
                self.cursor = (self.cursor + 1).min(ROWS.len() - 1);
                cx.notify();
            }
            ("k", modifiers) | ("up", modifiers) if modifiers == Modifiers::none() => {
                self.cursor = self.cursor.saturating_sub(1);
                cx.notify();
            }
            ("tab", modifiers) if modifiers == Modifiers::shift() => {
                self.cursor = self.cursor.saturating_sub(1);
                cx.notify();
            }
            ("enter", modifiers) | ("space", modifiers) if modifiers == Modifiers::none() => {
                if let Some(row) = ROWS.get(self.cursor).copied() {
                    self.activate(row, cx);
                }
            }
            ("escape", modifiers) if modifiers == Modifiers::none() => {
                cx.emit(SectionFocusEvent::RequestFocusReturn);
            }
            _ => {}
        }
    }
}

/// `<version> · <channel>`, as the running build reports itself.
fn status_detail() -> String {
    datatab_i18n::t!(
        "updates.settings.detail_unchecked",
        version = running_version_label(),
        channel = channel_label(ReleaseChannel::current())
    )
}

impl EventEmitter<SectionFocusEvent> for UpdatesSection {}

impl SettingsSection for UpdatesSection {
    fn section_id(&self) -> SettingsSectionId {
        SettingsSectionId::Updates
    }

    fn handle_key_event(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.handle_key(event, cx);
    }

    fn focus_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = true;
        cx.notify();
    }

    fn focus_out(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = false;
        cx.notify();
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.has_unsaved_changes(cx)
    }

    fn save_from_shortcut(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.save(cx);
    }

    fn render_footer_leading_actions(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        Some(self.render_button(
            "updates-reset",
            UpdatesRow::Reset,
            datatab_i18n::t!("updates.settings.reset"),
            AppIcon::RotateCcw,
            cx,
        ))
    }

    fn render_footer_actions(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        Some(
            Button::new("updates-save", datatab_i18n::t!("updates.settings.save"))
                .primary()
                .icon(AppIcon::Save)
                .when_some(crate::settings::save_shortcut(), Button::kbd)
                .focused(self.is_at(UpdatesRow::Save))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select(UpdatesRow::Save);
                    this.save(cx);
                }))
                .into_any_element(),
        )
    }
}

impl Render for UpdatesSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_body(cx);

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .flex_shrink_0()
                    .pb(crate::tokens::SettingsMetrics::PAGE_HEAD_PADDING_BOTTOM - Spacing::SM)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(datatab_components::composites::page_header(
                        datatab_i18n::t!("updates.settings.title"),
                        datatab_i18n::t!("updates.settings.subtitle"),
                        cx,
                    )),
            )
            .child(
                div()
                    .id("updates-settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px(crate::tokens::SettingsMetrics::BODY_PADDING_X)
                    .pb(Spacing::XL)
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent's `gpui::*` glob would make `#[test]`
    // resolve to `gpui::test`, whose expansion recurses without bound.
    use super::{ReleaseChannel, channel_label, running_version_label, status_detail};

    #[test]
    fn status_shows_the_version_and_channel() {
        let detail = status_detail();
        let version = running_version_label();
        let channel = channel_label(ReleaseChannel::current());

        assert!(detail.contains(&version), "{detail} lacks {version}");
        assert!(detail.contains(channel), "{detail} lacks {channel}");
    }

    #[test]
    fn updates_settings_copy_resolves() {
        for key in [
            "settings.nav.updates",
            "updates.settings.title",
            "updates.settings.subtitle",
            "updates.settings.group",
            "updates.settings.status",
            "updates.settings.detail_unchecked",
            "updates.settings.changelog",
            "updates.settings.show_whats_new",
            "updates.settings.open_whats_new",
            "updates.settings.show_welcome",
            "updates.settings.reset",
            "updates.settings.save",
            "updates.settings.saved",
            "updates.settings.save_error",
            "updates.ago.just_now",
            "updates.ago.minutes",
            "updates.ago.hours",
            "updates.ago.days",
        ] {
            let value = datatab_i18n::t!(key, locale = "en");

            assert!(!value.is_empty(), "{key} resolved empty");
            assert_ne!(value, format!("en.{key}"), "{key} missing");
        }
    }
}
