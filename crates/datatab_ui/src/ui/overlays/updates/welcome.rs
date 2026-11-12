use crate::keymap::{Command, ContextId, KeymapStack};
use datatab_components::controls::Button;
use datatab_components::modals::{Modal, ModalFocus};
use datatab_components::primitives::{Kbd, Text};
use datatab_components::tokens::{ChamferCut, ChromeColors, FontSizes, ui};
use datatab_core::{LogErr, ReleaseChannel};
use datatab_ui_base::keymap::{effective_keymap, key_sequence_label};
use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;

/// The wordmark: the product name, which is not translated.
const WORDMARK: &str = "DataTab";

const DIALOG_WIDTH: Rems = ui(560.0);
const MARK_SIZE: Rems = ui(48.0);
const WORDMARK_SIZE: Rems = ui(30.0);
const CARD_PADDING: Pixels = px(32.0);
const CARD_GAP: Pixels = px(20.0);
const HEAD_GAP: Pixels = px(14.0);
const KEY_COLUMN_WIDTH: Rems = ui(120.0);
const KEY_ROW_HEIGHT: Rems = ui(34.0);
const KEY_CELL_PADDING_RIGHT: Pixels = px(8.0);
const FOOTER_GAP: Pixels = px(16.0);

/// Element id of the dialog.
pub(crate) const DIALOG_ID: &str = "welcome-dialog";

/// Element id of the "Add a connection" button.
pub(crate) const ADD_CONNECTION_ID: &str = "welcome-add-connection";

/// What the welcome screen asks of the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WelcomeEvent {
    /// The user chose "Add a connection": the dialog has closed, and the
    /// workspace opens the Connection Manager on a new connection.
    AddConnection,
}

/// One of the keys the screen teaches: the command it shows, the key context
/// the keymap binds it in, and the text that says what it does.
struct KeyLesson {
    context: ContextId,
    command: Command,
    description_key: &'static str,
}

/// The three keys of the screen. The keys themselves are never written here:
/// they are read from the keymap, so a key the user rebinds changes on the
/// screen too.
const KEY_LESSONS: [KeyLesson; 3] = [
    KeyLesson {
        context: ContextId::Global,
        command: Command::ToggleCommandPalette,
        description_key: "updates.welcome.key_palette",
    },
    KeyLesson {
        context: ContextId::Global,
        command: Command::CycleFocusForward,
        description_key: "updates.welcome.key_next_pane",
    },
    KeyLesson {
        context: ContextId::Sidebar,
        command: Command::OpenItemMenu,
        description_key: "updates.welcome.key_item_menu",
    },
];

/// One row of the key list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaughtKey {
    id: SharedString,
    keys: SharedString,
    description: String,
}

/// The rows of the key list for `keymap`. A command the keymap does not bind
/// is left out, so the screen never teaches a key that does nothing.
fn taught_keys(keymap: &KeymapStack) -> Vec<TaughtKey> {
    KEY_LESSONS
        .iter()
        .filter_map(|lesson| {
            let keys = keymap.keys_for_command(lesson.context, lesson.command)?;

            Some(TaughtKey {
                id: format!("welcome-key-{}", lesson.command.id()).into(),
                keys: key_sequence_label(keys),
                description: datatab_i18n::t!(lesson.description_key),
            })
        })
        .collect()
}

/// First-run welcome: the mark, the wordmark, one sentence, the three keys
/// to learn first and a button that adds a connection. Settings › Updates
/// reopens it; the changelog lives in the "What is new" dialog.
pub struct WelcomeDialog {
    visible: bool,
    focus: ModalFocus,
    add_connection_focus: FocusHandle,
    focus_button_pending: bool,
}

impl EventEmitter<WelcomeEvent> for WelcomeDialog {}

impl WelcomeDialog {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            visible: false,
            focus: ModalFocus::new(cx),
            add_connection_focus: cx.focus_handle(),
            focus_button_pending: false,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.visible = true;
        self.focus_button_pending = true;
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }

        self.visible = false;
        self.focus_button_pending = false;
        self.focus.restore(cx);
        cx.notify();
    }

    /// Closes the screen and asks the workspace to open the Connection
    /// Manager. Enter and the button both end here.
    fn add_connection(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }

        self.close(cx);
        cx.emit(WelcomeEvent::AddConnection);
    }

    fn render_head(&self, cx: &App) -> impl IntoElement {
        let mark_path = match ReleaseChannel::current() {
            ReleaseChannel::Nightly => "branding/nightly/mark-256.png",
            ReleaseChannel::Stable | ReleaseChannel::Rc => "branding/stable/mark-256.png",
        };

        div()
            .flex()
            .items_center()
            .gap(HEAD_GAP)
            .child(img(mark_path).size(MARK_SIZE).flex_shrink_0())
            .child(
                div()
                    .font_family(datatab_components::fonts::display_family(cx))
                    .font_weight(FontWeight::EXTRA_BOLD)
                    .text_size(WORDMARK_SIZE)
                    .text_color(ChromeColors::strong(cx.theme()))
                    .child(WORDMARK),
            )
    }

    fn render_keys(&self, cx: &App) -> impl IntoElement {
        let divider = cx.theme().table_row_border;

        div()
            .flex()
            .flex_col()
            .children(taught_keys(&effective_keymap()).into_iter().map(|row| {
                div()
                    .flex()
                    .items_center()
                    .h(KEY_ROW_HEIGHT)
                    .border_b_1()
                    .border_color(divider)
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .min_w(KEY_COLUMN_WIDTH)
                            .pr(KEY_CELL_PADDING_RIGHT)
                            .child(Kbd::new(row.keys).id(row.id)),
                    )
                    .child(div().flex_1().min_w_0().child(Text::body(row.description)))
            }))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(FOOTER_GAP)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Text::caption(datatab_i18n::t!(
                        "updates.welcome.see_changes"
                    ))),
            )
            .child(
                Button::new(
                    ADD_CONNECTION_ID,
                    datatab_i18n::t!("updates.welcome.add_connection"),
                )
                .primary()
                .focus_handle(&self.add_connection_focus)
                .when_some(
                    datatab_ui_base::keymap::shortcut_label(ContextId::Modal, Command::Execute),
                    Button::kbd,
                )
                .on_click(cx.listener(|this, _, _, cx| this.add_connection(cx))),
            )
    }
}

impl Render for WelcomeDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().into_any_element();
        }

        // The button takes the focus, not the modal container, so the focus
        // ring shows on it. The modal still hears its keys from inside.
        if std::mem::take(&mut self.focus_button_pending) {
            self.focus
                .focus(Some(&self.add_connection_focus), window, cx);
        }

        // The board draws the card on the panel fill with no outline: its
        // ring is clipped away by the cut.
        let card = div()
            .flex()
            .flex_col()
            .gap(CARD_GAP)
            .p(CARD_PADDING)
            .child(self.render_head(cx))
            .child(Text::body(datatab_i18n::t!("updates.welcome.intro")).font_size(FontSizes::LG))
            .child(self.render_keys(cx))
            .child(self.render_footer(cx));

        let dialog = cx.entity().downgrade();
        let dialog_for_enter = dialog.clone();

        Modal::new(datatab_i18n::t!("updates.welcome.title"))
            .id(DIALOG_ID)
            .without_header()
            .width(DIALOG_WIDTH)
            .cut(ChamferCut::CARD)
            .border(gpui::transparent_black())
            .child(card)
            .focus_handle(self.focus.handle())
            .on_close(move |_, cx| {
                dialog.update(cx, |dialog, cx| dialog.close(cx)).log_err();
            })
            .on_confirm(move |_, cx| {
                dialog_for_enter
                    .update(cx, |dialog, cx| dialog.add_connection(cx))
                    .log_err();
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent's `gpui::*` glob would make `#[test]`
    // resolve to `gpui::test`, whose expansion recurses without bound.
    use super::{ADD_CONNECTION_ID, KEY_LESSONS, WelcomeDialog, WelcomeEvent, taught_keys};
    use crate::keymap::{Command, ContextId, KeymapLayer, KeymapStack};
    use datatab_app::keymap::{KeySequence, default_slots};
    use datatab_ui_base::keyboard_coverage::{
        Coverage, FrameCapture, KeyboardPath, SurfaceRegistry,
    };
    use datatab_ui_base::keymap::{
        apply_keymap_overrides, default_keymap, effective_keymap, keymap_overrides,
        keymap_state_test_guard, shortcut_label,
    };
    use gpui::{
        Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    const PALETTE_ROW: &str = "welcome-key-toggle_command_palette";
    const NEXT_PANE_ROW: &str = "welcome-key-cycle_focus_forward";
    const ITEM_MENU_ROW: &str = "welcome-key-open_item_menu";

    /// The welcome dialog's registry (see `datatab_ui_base::keyboard_coverage`).
    /// The key rows and the footer note are not interactive.
    const WELCOME: SurfaceRegistry = SurfaceRegistry {
        name: "Welcome dialog",
        contexts: &[ContextId::Modal],
        entries: &[(ADD_CONNECTION_ID, KeyboardPath::Command(Command::Execute))],
    };

    /// A keymap that binds the three commands of the screen to keys no
    /// default keymap uses.
    fn unusual_keymap() -> KeymapStack {
        let bind = |layer: &mut KeymapLayer, keys: &str, command: Command| {
            layer.bind(KeySequence::parse(keys).expect("valid keys"), command);
        };

        let mut global = KeymapLayer::new(ContextId::Global);
        bind(&mut global, "ctrl+k p", Command::ToggleCommandPalette);
        bind(&mut global, "f7", Command::CycleFocusForward);

        let mut sidebar = KeymapLayer::new(ContextId::Sidebar);
        bind(&mut sidebar, "ctrl+alt+q", Command::OpenItemMenu);

        let mut keymap = KeymapStack::new();
        keymap.add_layer(global);
        keymap.add_layer(sidebar);
        keymap
    }

    fn shown_keys(keymap: &KeymapStack) -> Vec<(String, String)> {
        taught_keys(keymap)
            .into_iter()
            .map(|row| (row.id.to_string(), row.keys.to_string()))
            .collect()
    }

    /// Fails when a key is written into the screen instead of read from the
    /// keymap: whatever the keymap binds is what the rows show.
    #[test]
    fn the_rows_show_the_keys_the_keymap_binds() {
        assert_eq!(
            shown_keys(&unusual_keymap()),
            [
                (PALETTE_ROW.to_string(), "Ctrl K  p".to_string()),
                (NEXT_PANE_ROW.to_string(), "F7".to_string()),
                (ITEM_MENU_ROW.to_string(), "Ctrl Alt Q".to_string()),
            ],
            "the rows must show the keys of the keymap they were built from, not fixed keys"
        );
    }

    #[test]
    fn a_command_without_a_key_is_left_out() {
        let mut global = KeymapLayer::new(ContextId::Global);
        global.bind(
            KeySequence::parse("f7").expect("valid keys"),
            Command::CycleFocusForward,
        );
        let mut keymap = KeymapStack::new();
        keymap.add_layer(global);

        assert_eq!(
            shown_keys(&keymap),
            [(NEXT_PANE_ROW.to_string(), "F7".to_string())],
            "a command the keymap does not bind must not be taught"
        );
    }

    #[test]
    fn the_default_keymap_binds_all_three_keys() {
        let ids: Vec<String> = shown_keys(&effective_keymap())
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert_eq!(
            ids,
            [PALETTE_ROW, NEXT_PANE_ROW, ITEM_MENU_ROW],
            "each of the three commands must be bound by default"
        );
        assert_eq!(KEY_LESSONS.len(), 3);
    }

    /// The screen's code holds no key name: a literal like these would show
    /// a key the user may have rebound.
    #[test]
    fn the_screen_source_spells_out_no_key() {
        let source = include_str!("welcome.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);

        for literal in [
            "\"Ctrl",
            "\"Shift",
            "\"Alt",
            "\"Cmd",
            "\"Tab\"",
            "\"Enter\"",
            "\"Esc",
        ] {
            assert!(
                !production.contains(literal),
                "welcome.rs writes the key {literal} by hand: read it from the keymap"
            );
        }
    }

    type Opened<'a> = (
        Entity<WelcomeDialog>,
        &'a mut VisualTestContext,
        Rc<RefCell<Vec<WelcomeEvent>>>,
    );

    /// An open welcome dialog in a window, and the events it emits.
    fn open_dialog(cx: &mut TestAppContext) -> Opened<'_> {
        cx.update(datatab_components::theme::init);
        cx.update(datatab_ui_base::keymap::init_keymap);

        let (dialog, window) = cx.add_window_view(|_, cx| WelcomeDialog::new(cx));
        let events = Rc::new(RefCell::new(Vec::new()));

        window.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&dialog, move |_, event: &WelcomeEvent, _| {
                events.borrow_mut().push(*event);
            })
            .detach();
        });

        dialog.update(window, |dialog, cx| dialog.open(cx));
        window.run_until_parked();

        (dialog, window, events)
    }

    fn is_visible(dialog: &Entity<WelcomeDialog>, window: &mut VisualTestContext) -> bool {
        dialog.read_with(window, |dialog, _| dialog.is_visible())
    }

    /// The text of the keycap `id` as the accessibility tree reports it.
    fn key_name(capture: &FrameCapture, window: &mut VisualTestContext, id: &str) -> String {
        let frame = capture.frame(window);
        let (_, node) = frame
            .nodes()
            .find(|(_, node)| node.id() == id)
            .unwrap_or_else(|| panic!("the row {id} is not on the screen"));

        frame
            .accessibility_node(node)
            .and_then(|accessible| accessible.label().map(str::to_owned))
            .unwrap_or_else(|| panic!("the keycap {id} has no accessible name"))
    }

    /// A key the user rebinds shows on the next frame, and the old key is
    /// gone.
    #[gpui::test]
    fn a_rebound_key_shows_on_the_screen(cx: &mut TestAppContext) {
        let _keymap_state = keymap_state_test_guard();
        let (_dialog, window, _events) = open_dialog(cx);
        let capture = FrameCapture::observe(window);

        for (row, command, context) in [
            (
                PALETTE_ROW,
                Command::ToggleCommandPalette,
                ContextId::Global,
            ),
            (NEXT_PANE_ROW, Command::CycleFocusForward, ContextId::Global),
            (ITEM_MENU_ROW, Command::OpenItemMenu, ContextId::Sidebar),
        ] {
            let bound = shortcut_label(context, command).expect("bound by default");
            assert_eq!(
                key_name(&capture, window, row),
                bound.to_string(),
                "{row} must show the key the keymap binds to {}",
                command.id()
            );
        }

        let slot = default_slots(default_keymap())
            .into_iter()
            .find(|slot| {
                slot.context == ContextId::Sidebar && slot.command == Command::OpenItemMenu
            })
            .expect("the sidebar binds the item menu by default");
        let mut overrides = keymap_overrides();
        overrides.set(
            slot,
            Some(KeySequence::parse("ctrl+alt+q").expect("valid keys")),
        );
        window.update(|_, cx| apply_keymap_overrides(overrides, cx));
        window.run_until_parked();

        assert_eq!(
            key_name(&capture, window, ITEM_MENU_ROW),
            "Ctrl Alt Q",
            "the screen must show the rebound key"
        );
    }

    #[gpui::test]
    fn enter_on_the_container_closes_the_screen_and_asks_for_a_connection(cx: &mut TestAppContext) {
        let (dialog, window, events) = open_and_drawn(cx);
        let container = dialog.read_with(window, |dialog, _| dialog.focus.handle().clone());
        window.update(|window, cx| container.focus(window, cx));
        window.run_until_parked();

        window.simulate_keystrokes("enter");
        window.run_until_parked();

        assert!(!is_visible(&dialog, window), "Enter must close the screen");
        assert_eq!(
            *events.borrow(),
            [WelcomeEvent::AddConnection],
            "Enter must ask the workspace to add a connection, once"
        );
    }

    #[gpui::test]
    fn the_button_closes_the_screen_and_asks_for_a_connection(cx: &mut TestAppContext) {
        let (dialog, window, events) = open_dialog(cx);

        window.update(|window, _| window.refresh());
        window.run_until_parked();
        let button = window
            .debug_bounds(ADD_CONNECTION_ID)
            .expect("the Add a connection button is on the screen");
        window.simulate_click(button.center(), Modifiers::none());
        window.run_until_parked();

        assert!(
            !is_visible(&dialog, window),
            "the button must close the screen"
        );
        assert_eq!(
            *events.borrow(),
            [WelcomeEvent::AddConnection],
            "the button must ask the workspace to add a connection, once"
        );
    }

    #[gpui::test]
    fn escape_closes_the_screen_without_adding_a_connection(cx: &mut TestAppContext) {
        let (dialog, window, events) = open_dialog(cx);

        window.simulate_keystrokes("escape");
        window.run_until_parked();

        assert!(!is_visible(&dialog, window), "Escape must close the screen");
        assert!(
            events.borrow().is_empty(),
            "Escape must not ask for a connection: {:?}",
            events.borrow()
        );
    }

    /// Whether the "Add a connection" button holds keyboard focus.
    fn button_is_focused(dialog: &Entity<WelcomeDialog>, window: &mut VisualTestContext) -> bool {
        let handle = dialog.read_with(window, |dialog, _| dialog.add_connection_focus.clone());
        window.update(|window, _| handle.is_focused(window))
    }

    /// Presses and releases Enter, as a keyboard does: a focused button
    /// takes the key itself and activates on the release.
    fn press_enter(window: &mut VisualTestContext) {
        let keystroke = Keystroke::parse("enter").expect("valid keystroke");

        window.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        window.simulate_event(KeyUpEvent { keystroke });
    }

    /// An open welcome dialog that has drawn once, so its pending focus has
    /// been applied.
    fn open_and_drawn(cx: &mut TestAppContext) -> Opened<'_> {
        let (dialog, window, events) = open_dialog(cx);
        window.update(|window, _| window.refresh());
        window.run_until_parked();
        (dialog, window, events)
    }

    /// Fails when the dialog focuses its container instead of the button:
    /// the focus ring would then not show on the one control.
    #[gpui::test]
    fn opening_focuses_the_add_connection_button(cx: &mut TestAppContext) {
        let (dialog, window, _events) = open_and_drawn(cx);

        assert!(
            button_is_focused(&dialog, window),
            "the Add a connection button must hold focus once the dialog has drawn"
        );
    }

    /// With the button focused, its own activation and the modal's Enter
    /// binding must not both add a connection.
    #[gpui::test]
    fn enter_on_the_focused_button_adds_one_connection(cx: &mut TestAppContext) {
        let (dialog, window, events) = open_and_drawn(cx);
        assert!(
            button_is_focused(&dialog, window),
            "the button must hold focus before Enter"
        );

        press_enter(window);
        window.run_until_parked();

        assert!(!is_visible(&dialog, window), "Enter must close the screen");
        assert_eq!(
            *events.borrow(),
            [WelcomeEvent::AddConnection],
            "Enter on the focused button must add exactly one connection"
        );
    }

    #[gpui::test]
    fn escape_on_the_focused_button_closes_without_adding(cx: &mut TestAppContext) {
        let (dialog, window, events) = open_and_drawn(cx);
        assert!(
            button_is_focused(&dialog, window),
            "the button must hold focus before Escape"
        );

        window.simulate_keystrokes("escape");
        window.run_until_parked();

        assert!(!is_visible(&dialog, window), "Escape must close the screen");
        assert!(
            events.borrow().is_empty(),
            "Escape must not ask for a connection: {:?}",
            events.borrow()
        );
    }

    /// The one control of the screen runs on Enter, which the Modal key
    /// context binds to Execute.
    #[gpui::test]
    fn the_welcome_dialog_is_covered(cx: &mut TestAppContext) {
        let (_dialog, window, _events) = open_dialog(cx);
        let capture = FrameCapture::observe(window);

        let covered = Coverage::new(WELCOME).assert_covered(&capture.frame(window));

        assert!(
            covered.iter().any(|id| id == ADD_CONNECTION_ID),
            "the button was not drawn: {covered:?}"
        );
    }

    #[test]
    fn welcome_copy_resolves() {
        for key in [
            "updates.welcome.title",
            "updates.welcome.intro",
            "updates.welcome.key_palette",
            "updates.welcome.key_next_pane",
            "updates.welcome.key_item_menu",
            "updates.welcome.see_changes",
            "updates.welcome.add_connection",
        ] {
            let value = datatab_i18n::t!(key, locale = "en");
            assert_ne!(value, format!("en.{key}"), "{key} missing");
        }
    }
}
