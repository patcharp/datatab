use crate::keymap::{RunCommand, run_command};
use datatab_app::keymap::{Command, ContextId};
use datatab_components::actions::{ScrollDown, ScrollToBottom, ScrollToTop, ScrollUp};
use datatab_components::chart::ChartKind;
use datatab_components::composites::{inline_tab, inline_tab_bar};
use datatab_components::controls::{Button, GpuiInput as Input, InputEvent, InputState};
use datatab_components::icons::AppIcon;
use datatab_components::modals::modal::{Modal, ModalFocus};
use datatab_components::primitives::Text;
use datatab_components::saved_chart::SavedChart;
use datatab_components::tokens::{FontSizes, Heights, Radii, Spacing};
use datatab_components::vim::{VimBinding, VimHost};
use datatab_core::LogErr;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, Render, Subscription, Window, div, px,
};
use gpui_component::ActiveTheme;
use gpui_component::input::EditorState;
use uuid::Uuid;

impl VimHost for ModalAddPanelPicker {
    fn vim(&self, input: EntityId) -> Option<&VimBinding> {
        self.query_vim.for_input(input)
    }

    fn vim_mut(&mut self, input: EntityId) -> Option<&mut VimBinding> {
        self.query_vim.for_input_mut(input)
    }
}

/// Outcome emitted when the user resolves the add-panel picker.
#[derive(Clone, Debug)]
pub enum AddPanelOutcome {
    /// User confirmed selection of one or more existing saved charts.
    Confirmed {
        dashboard_id: Uuid,
        chart_ids: Vec<Uuid>,
    },
    /// User submitted the "From query" tab and wants a new query-backed panel.
    CreateFromQuery {
        dashboard_id: Uuid,
        profile_id: Uuid,
        name: String,
        query: String,
        chart_kind: ChartKind,
    },
    Cancelled,
}

/// Request payload for opening the add-panel picker.
#[derive(Clone, Debug)]
pub struct AddPanelRequest {
    pub dashboard_id: Uuid,
    /// Profile of the active connection backing this dashboard.
    pub profile_id: Uuid,
    /// All saved charts available for this profile's connection.
    pub candidates: Vec<SavedChart>,
}

/// Active tab in the picker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AddPanelTab {
    #[default]
    Saved,
    Query,
}

/// Pure helper: submit button label given an active tab and current
/// saved-tab selection count. Extracted to keep tests GPUI-free.
pub fn submit_label_for(tab: AddPanelTab, saved_selection_count: usize) -> String {
    match tab {
        AddPanelTab::Saved => match saved_selection_count {
            0 => datatab_i18n::t!("modals.add_panel_picker.submit.zero"),
            1 => datatab_i18n::t!("modals.add_panel_picker.submit.one"),
            n => datatab_i18n::t!("modals.add_panel_picker.submit.many", count = n),
        },
        AddPanelTab::Query => datatab_i18n::t!("modals.add_panel_picker.submit.create"),
    }
}

/// Pure helper: catalog key for a chart kind's translated label.
fn chart_kind_label_key(kind: ChartKind) -> &'static str {
    match kind {
        ChartKind::Line => "modals.add_panel_picker.chart_kind.line",
        ChartKind::Bar => "modals.add_panel_picker.chart_kind.bar",
        ChartKind::Area => "modals.add_panel_picker.chart_kind.area",
        ChartKind::Scatter => "modals.add_panel_picker.chart_kind.scatter",
        _ => "modals.add_panel_picker.chart_kind.line",
    }
}

/// Translated label for a chart kind, using the active process-wide locale.
fn chart_kind_label(kind: ChartKind) -> String {
    datatab_i18n::t!(chart_kind_label_key(kind))
}

/// Pure helper: query-tab form validity.
pub fn query_tab_valid(name: &str, query: &str) -> bool {
    !name.trim().is_empty() && !query.trim().is_empty()
}

/// Modal entity for adding panels to a dashboard.
///
/// Two tabs:
/// - `Saved` — pick from existing saved charts (current behaviour).
/// - `Query` — type a SQL query and pick a chart kind to spawn a new panel.
pub struct ModalAddPanelPicker {
    request: Option<AddPanelRequest>,
    visible: bool,
    active_tab: AddPanelTab,
    focus: ModalFocus,

    // Saved-tab state.
    search_input: Entity<InputState>,
    selected_ids: Vec<Uuid>,
    /// Row of the chart list the keyboard is on (into the filtered list).
    saved_highlight: usize,
    /// Tab stop of the chart list: the list keys work while it has focus.
    saved_list_focus: FocusHandle,

    // Query-tab state.
    query_name_input: Entity<InputState>,
    query_input: Entity<EditorState>,
    /// Vim mode for the query editor.
    query_vim: VimBinding,
    query_chart_kind: ChartKind,

    _subscriptions: Vec<Subscription>,
}

impl ModalAddPanelPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(datatab_i18n::t!(
                "modals.add_panel_picker.saved.search_placeholder"
            ))
        });
        let query_name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(datatab_i18n::t!("modals.add_panel_picker.name_placeholder"))
        });
        let query_input = cx.new(|cx| {
            // soft_wrap defaults to true in 0.6.1, so the old explicit builder is gone.
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder(datatab_i18n::t!(
                    "modals.add_panel_picker.query.placeholder"
                ))
        });
        let query_vim = VimBinding::new(query_input.clone(), window, cx);

        let mut picker = Self {
            request: None,
            visible: false,
            active_tab: AddPanelTab::default(),
            focus: ModalFocus::new(cx),
            search_input,
            selected_ids: Vec::new(),
            saved_highlight: 0,
            saved_list_focus: cx.focus_handle().tab_stop(true),
            query_name_input,
            query_input,
            query_vim,
            query_chart_kind: ChartKind::Line,
            _subscriptions: Vec::new(),
        };

        let input = picker.query_vim.input_id();
        VimBinding::follow_setting(&mut picker, input, cx);
        picker
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn active_tab(&self) -> AddPanelTab {
        self.active_tab
    }

    pub fn set_active_tab(&mut self, tab: AddPanelTab, cx: &mut Context<Self>) {
        self.active_tab = tab;
        cx.notify();
    }

    pub fn open(&mut self, request: AddPanelRequest, window: &mut Window, cx: &mut Context<Self>) {
        self.request = Some(request);
        self.visible = true;
        self.active_tab = AddPanelTab::Saved;
        self.selected_ids.clear();
        self.saved_highlight = 0;
        self.query_chart_kind = ChartKind::Line;

        self.search_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.query_name_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.query_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        let mut subs = Vec::new();
        for input in [&self.search_input, &self.query_name_input] {
            let sub = cx.subscribe_in(input, window, |_this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
            subs.push(sub);
        }
        // EditorState is a distinct entity type in 0.6.1, so the query input
        // subscribes separately with the same handler.
        subs.push(cx.subscribe_in(
            &self.query_input,
            window,
            |_this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        self._subscriptions = subs;

        let search_focus = self.search_input.read(cx).focus_handle(cx);
        self.focus.focus(Some(&search_focus), window, cx);

        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        self.request = None;
        self.selected_ids.clear();
        self.focus.restore(cx);
        cx.notify();
    }

    /// Dismiss the picker without adding anything.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(AddPanelOutcome::Cancelled);
        self.close(cx);
    }

    pub fn toggle_chart(&mut self, chart_id: Uuid, cx: &mut Context<Self>) {
        if let Some(pos) = self.selected_ids.iter().position(|id| *id == chart_id) {
            self.selected_ids.remove(pos);
        } else {
            self.selected_ids.push(chart_id);
        }
        cx.notify();
    }

    /// Returns the submit button label based on the active tab and state.
    pub fn submit_label(&self) -> String {
        submit_label_for(self.active_tab, self.selected_ids.len())
    }

    /// Returns true when the current tab's form passes validation.
    pub fn can_confirm(&self, cx: &App) -> bool {
        match self.active_tab {
            AddPanelTab::Saved => !self.selected_ids.is_empty(),
            AddPanelTab::Query => {
                let name = self.query_name_input.read(cx).value().to_string();
                let query = self.query_input.read(cx).value().to_string();
                query_tab_valid(&name, &query)
            }
        }
    }

    pub fn visible_tabs(&self) -> Vec<AddPanelTab> {
        vec![AddPanelTab::Saved, AddPanelTab::Query]
    }

    /// Returns filtered candidates matching the current search query (case-insensitive).
    fn filtered_candidates<'a>(candidates: &'a [SavedChart], query: &str) -> Vec<&'a SavedChart> {
        if query.is_empty() {
            return candidates.iter().collect();
        }
        let lower = query.to_lowercase();
        candidates
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&lower))
            .collect()
    }

    /// Submit the active tab, as the submit button does. Does nothing while
    /// the tab's form is incomplete.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) {
            return;
        }
        let Some(ref request) = self.request else {
            return;
        };

        match self.active_tab {
            AddPanelTab::Saved => {
                cx.emit(AddPanelOutcome::Confirmed {
                    dashboard_id: request.dashboard_id,
                    chart_ids: self.selected_ids.clone(),
                });
            }
            AddPanelTab::Query => {
                let name = self.query_name_input.read(cx).value().to_string();
                let query = self.query_input.read(cx).value().to_string();
                cx.emit(AddPanelOutcome::CreateFromQuery {
                    dashboard_id: request.dashboard_id,
                    profile_id: request.profile_id,
                    name: name.trim().to_string(),
                    query,
                    chart_kind: self.query_chart_kind,
                });
            }
        }

        self.close(cx);
    }

    // ---- keyboard ----

    /// Ids of the saved charts the search leaves in the list, in order.
    fn filtered_saved_ids(&self, cx: &App) -> Vec<Uuid> {
        let Some(request) = self.request.as_ref() else {
            return Vec::new();
        };
        let query = self.search_input.read(cx).value().to_string();

        Self::filtered_candidates(&request.candidates, &query)
            .into_iter()
            .map(|chart| chart.id)
            .collect()
    }

    /// Selects the tab `delta` places away among the visible ones, wrapping,
    /// and moves the keyboard to its first field: the field that had it is
    /// no longer drawn.
    fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.visible_tabs();
        let current = tabs
            .iter()
            .position(|tab| *tab == self.active_tab)
            .unwrap_or(0);
        #[expect(
            clippy::indexing_slicing,
            reason = "`visible_tabs` always returns at least the Saved and Query tabs, and `rem_euclid` over the positive `tabs.len()` yields an index in `[0, tabs.len())`"
        )]
        let next = tabs[(current as isize + delta).rem_euclid(tabs.len() as isize) as usize];

        self.set_active_tab(next, cx);

        let first_field = match next {
            AddPanelTab::Saved => self.search_input.clone(),
            AddPanelTab::Query => self.query_name_input.clone(),
        };
        first_field.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Moves the highlighted row of the list the keyboard works by `delta`,
    /// or to its first (`Some(false)`) or last (`Some(true)`) row.
    fn move_highlight(
        &mut self,
        delta: isize,
        to_edge: Option<bool>,
        _window: &Window,
        cx: &mut Context<Self>,
    ) {
        let (count, highlight) = match self.active_tab {
            AddPanelTab::Saved => (self.filtered_saved_ids(cx).len(), &mut self.saved_highlight),
            AddPanelTab::Query => return,
        };

        if count == 0 {
            return;
        }

        *highlight = match to_edge {
            Some(true) => count - 1,
            Some(false) => 0,
            None => (*highlight)
                .min(count - 1)
                .saturating_add_signed(delta)
                .min(count - 1),
        };
        cx.notify();
    }

    /// Space: checks or unchecks the highlighted chart.
    fn toggle_highlighted(&mut self, _window: &Window, cx: &mut Context<Self>) {
        if self.active_tab == AddPanelTab::Saved
            && let Some(id) = self
                .filtered_saved_ids(cx)
                .get(self.saved_highlight)
                .copied()
        {
            self.toggle_chart(id, cx);
        }
    }

    /// Whether Enter has something to do: the tab's form is complete, or a
    /// highlighted row Enter would pick first.
    fn can_confirm_from_keys(&self, cx: &App) -> bool {
        self.can_confirm(cx)
            || match self.active_tab {
                AddPanelTab::Saved => {
                    self.selected_ids.is_empty() && !self.filtered_saved_ids(cx).is_empty()
                }
                AddPanelTab::Query => false,
            }
    }

    /// Enter: in the chart list with nothing checked, adds the highlighted
    /// chart, then submits once the form is complete.
    fn confirm_from_keys(&mut self, _window: &Window, cx: &mut Context<Self>) {
        if self.active_tab == AddPanelTab::Saved
            && self.selected_ids.is_empty()
            && let Some(id) = self
                .filtered_saved_ids(cx)
                .get(self.saved_highlight)
                .copied()
        {
            self.selected_ids.push(id);
        }

        self.confirm(cx);
    }

    /// `/`: moves the keyboard to the first field of the tab.
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.active_tab {
            AddPanelTab::Saved => self.search_input.clone(),
            AddPanelTab::Query => self.query_name_input.clone(),
        };

        input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Runs a keymap command of the dialog's own layer (see the module's
    /// `AddPanelPicker` keys). Returns whether it applied.
    fn run_key(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match command {
            Command::NextPanelTab => self.step_tab(1, window, cx),
            Command::PrevPanelTab => self.step_tab(-1, window, cx),
            Command::SelectNext => self.move_highlight(1, None, window, cx),
            Command::SelectPrev => self.move_highlight(-1, None, window, cx),
            Command::SelectFirst => self.move_highlight(0, Some(false), window, cx),
            Command::SelectLast => self.move_highlight(0, Some(true), window, cx),
            Command::ExpandCollapse => self.toggle_highlighted(window, cx),
            Command::FocusSearch => self.focus_search(window, cx),
            _ => return false,
        }

        true
    }

    fn render_chart_row(
        chart_id: uuid::Uuid,
        chart_name: String,
        is_selected: bool,
        is_highlighted: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();

        let checkbox_bg = if is_selected {
            theme.primary
        } else {
            theme.background
        };
        let checkbox_border = if is_selected {
            theme.primary
        } else {
            datatab_components::colors::Colors::for_current(cx).field_edge
        };

        let mut checkbox = div()
            .w(Heights::ICON_SM)
            .h(Heights::ICON_SM)
            .border_1()
            .border_color(checkbox_border)
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .bg(checkbox_bg);

        if is_selected {
            checkbox = checkbox.child(
                datatab_components::primitives::Text::caption("✓").color(theme.primary_foreground),
            );
        }

        let row_bg = if is_selected {
            theme.accent.opacity(0.18)
        } else {
            gpui::transparent_black()
        };
        let hover_bg = theme.secondary;

        let row_id = ("add-panel-chart-row", chart_id.as_u128() as u64);

        div()
            .id(row_id)
            .flex()
            .items_center()
            .gap(Spacing::SM)
            .px(Spacing::SM)
            .py(Spacing::XS)
            .rounded(datatab_components::tokens::Radii::SM)
            .bg(row_bg)
            .border_1()
            .border_color(if is_highlighted {
                theme.ring
            } else {
                gpui::transparent_black()
            })
            .when(!is_selected, |el| el.hover(move |d| d.bg(hover_bg)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_chart(chart_id, cx);
            }))
            .child(checkbox.into_any_element())
            .child(div().flex_1().text_sm().child(chart_name))
            .into_any_element()
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let visible = self.visible_tabs();
        let active = self.active_tab;

        let tabs: Vec<AnyElement> = visible
            .into_iter()
            .map(|tab| {
                let label = match tab {
                    AddPanelTab::Saved => datatab_i18n::t!("modals.add_panel_picker.tab.saved"),
                    AddPanelTab::Query => datatab_i18n::t!("modals.add_panel_picker.tab.query"),
                };
                let id = match tab {
                    AddPanelTab::Saved => "add-panel-tab-saved",
                    AddPanelTab::Query => "add-panel-tab-query",
                };
                let is_active = tab == active;

                inline_tab(id, is_active, cx)
                    .role(gpui::Role::Tab)
                    .aria_selected(is_active)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_active_tab(tab, cx);
                    }))
                    .child(Text::body(label))
                    .into_any_element()
            })
            .collect();

        inline_tab_bar(cx)
            .id("add-panel-tab-list")
            .role(gpui::Role::TabList)
            .children(tabs)
            .into_any_element()
    }

    fn render_saved_tab(&self, candidates: &[SavedChart], cx: &mut Context<Self>) -> AnyElement {
        if candidates.is_empty() {
            return Text::body(datatab_i18n::t!("modals.add_panel_picker.saved.empty"))
                .into_any_element();
        }

        let query = self.search_input.read(cx).value().to_string();
        let filtered = Self::filtered_candidates(candidates, &query);
        let selected_ids = self.selected_ids.clone();

        let search_row = div()
            .flex()
            .flex_col()
            .gap(Spacing::XS)
            .child(
                Text::body(datatab_i18n::t!(
                    "modals.add_panel_picker.saved.search_label"
                ))
                .into_any_element(),
            )
            .child(Input::new(&self.search_input))
            .into_any_element();

        let highlight = self.saved_highlight.min(filtered.len().saturating_sub(1));
        let chart_rows: Vec<AnyElement> = filtered
            .iter()
            .enumerate()
            .map(|(row, chart)| {
                let is_selected = selected_ids.contains(&chart.id);
                Self::render_chart_row(
                    chart.id,
                    chart.name.clone(),
                    is_selected,
                    row == highlight,
                    cx,
                )
            })
            .collect();

        // A tab stop after the search: J / K, Space and Enter work the list
        // from here, and the arrows work it from the search too.
        let chart_list = div()
            .id("add-panel-chart-list")
            .track_focus(&self.saved_list_focus)
            .flex()
            .flex_col()
            .gap(Spacing::XS)
            .children(chart_rows)
            .into_any_element();

        div()
            .flex()
            .flex_col()
            .gap(Spacing::SM)
            .child(search_row)
            .child(chart_list)
            .into_any_element()
    }

    fn render_chart_kind_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let kinds = [
            (ChartKind::Line, "add-panel-kind-line"),
            (ChartKind::Bar, "add-panel-kind-bar"),
            (ChartKind::Area, "add-panel-kind-area"),
            (ChartKind::Scatter, "add-panel-kind-scatter"),
        ];
        let current = self.query_chart_kind;

        let buttons: Vec<AnyElement> = kinds
            .into_iter()
            .map(|(kind, id)| {
                let label = chart_kind_label(kind);
                let is_active = kind == current;
                let mut btn =
                    Button::new(id, label).on_click(cx.listener(move |this, _, _, cx| {
                        this.query_chart_kind = kind;
                        cx.notify();
                    }));
                if is_active {
                    btn = btn.primary();
                } else {
                    btn = btn.ghost();
                }
                btn.into_any_element()
            })
            .collect();

        div()
            .flex()
            .items_center()
            .gap(Spacing::XS)
            .children(buttons)
            .into_any_element()
    }

    fn render_query_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        // Vim's listeners on the query editor's container. In Insert mode the
        // dialog's Escape leaves Insert mode instead of cancelling, and in
        // Normal mode Enter moves down instead of submitting.
        let input = self.query_vim.input_id();
        let query_container = VimBinding::wire(self.query_vim.leader_scope(div(), cx), input, cx);
        let query_container = VimBinding::capture_action::<datatab_components::actions::Cancel, _>(
            query_container,
            input,
            cx,
        );
        let query_container = VimBinding::capture_action::<datatab_components::actions::Execute, _>(
            query_container,
            input,
            cx,
        );

        let name_row = div()
            .flex()
            .flex_col()
            .gap(Spacing::XS)
            .child(
                Text::body(datatab_i18n::t!("modals.add_panel_picker.name_label"))
                    .into_any_element(),
            )
            .child(Input::new(&self.query_name_input))
            .into_any_element();

        let theme = cx.theme();
        let query_row = div()
            .flex()
            .flex_col()
            .gap(Spacing::XS)
            .child(
                Text::body(datatab_i18n::t!("modals.add_panel_picker.query.label"))
                    .into_any_element(),
            )
            .child(
                query_container
                    .border_1()
                    .border_color(theme.border)
                    .rounded(Radii::SM)
                    .bg(theme.background)
                    .h(px(320.0))
                    .p(Spacing::SM)
                    .overflow_hidden()
                    .child(
                        self.query_vim
                            .editor(false)
                            .w_full()
                            .h_full()
                            .font_family(datatab_components::fonts::editor_family(cx))
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(FontSizes::BASE),
                    )
                    .into_any_element(),
            )
            .children(self.query_vim.render_indicator(cx))
            .into_any_element();

        let kind_row = div()
            .flex()
            .flex_col()
            .gap(Spacing::XS)
            .child(
                Text::body(datatab_i18n::t!("modals.add_panel_picker.chart_kind.label"))
                    .into_any_element(),
            )
            .child(self.render_chart_kind_picker(cx))
            .into_any_element();

        div()
            .flex()
            .flex_col()
            .gap(Spacing::LG)
            .child(name_row)
            .child(query_row)
            .child(kind_row)
            .into_any_element()
    }
}

impl EventEmitter<AddPanelOutcome> for ModalAddPanelPicker {}

impl Render for ModalAddPanelPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().into_any_element();
        }

        let Some(ref request) = self.request else {
            return div().into_any_element();
        };

        let candidates = request.candidates.clone();
        let submit_label = self.submit_label();
        let can_confirm = self.can_confirm(cx);
        let can_confirm_from_keys = self.can_confirm_from_keys(cx);

        let tab_strip = self.render_tab_strip(cx);
        let body_inner: AnyElement = match self.active_tab {
            AddPanelTab::Saved => self.render_saved_tab(&candidates, cx),
            AddPanelTab::Query => self.render_query_tab(cx),
        };

        let body = div()
            .flex()
            .flex_col()
            .gap(Spacing::LG)
            .child(tab_strip)
            .child(body_inner)
            .into_any_element();

        let on_cancel = cx.listener(|this, _: &gpui::ClickEvent, _, cx| this.cancel(cx));
        let on_confirm = cx.listener(|this, _: &gpui::ClickEvent, _, cx| this.confirm(cx));

        let footer = div()
            .flex()
            .items_center()
            .gap(Spacing::SM)
            .child(
                Button::new(
                    "add-panel-cancel",
                    datatab_i18n::t!("modals.add_panel_picker.cancel"),
                )
                .on_click(on_cancel),
            )
            .child(
                Button::new("add-panel-confirm", submit_label)
                    .primary()
                    .disabled(!can_confirm)
                    .on_click(on_confirm),
            );

        let modal = Modal::new(datatab_i18n::t!("modals.add_panel_picker.title"))
            .body(body)
            .footer(footer)
            .icon(AppIcon::Plus)
            .width(gpui::px(900.0))
            .focus_handle(self.focus.handle())
            .key_context(ContextId::AddPanelPicker.as_gpui_context())
            .on_close({
                let entity = cx.entity().downgrade();
                move |_, cx| {
                    entity.update(cx, |this, cx| this.cancel(cx)).log_err();
                }
            })
            .on_confirm({
                let entity = cx.entity().downgrade();
                move |window, cx| {
                    entity
                        .update(cx, |this, cx| this.confirm_from_keys(window, cx))
                        .log_err();
                }
            })
            .confirm_enabled(can_confirm_from_keys);

        // The dialog's keys: its own layer's commands, and the arrow and
        // Home / End keys the modal answers only when it scrolls, which move
        // through the lists here (also from the search field).
        div()
            .id("add-panel-picker")
            .absolute()
            .inset_0()
            .on_action(cx.listener(|this, action: &RunCommand, window, cx| {
                let handled =
                    run_command(action).is_some_and(|command| this.run_key(command, window, cx));

                if !handled {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ScrollDown, window, cx| {
                this.move_highlight(1, None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollUp, window, cx| {
                this.move_highlight(-1, None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollToTop, window, cx| {
                this.move_highlight(0, Some(false), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollToBottom, window, cx| {
                this.move_highlight(0, Some(true), window, cx);
            }))
            .child(modal)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datatab_components::chart::ChartSpec;
    use datatab_components::saved_chart::{SavedChart, SavedChartRefreshPolicy, SavedChartSource};

    fn test_uuid() -> Uuid {
        Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
    }

    fn make_chart(name: &str, profile_id: Uuid) -> SavedChart {
        let chart_spec: ChartSpec = serde_json::from_str(
            r#"{"x_axis":{"column_index":0,"label":"t","kind":"Time","unit":null},"series":[]}"#,
        )
        .unwrap();

        SavedChart {
            id: Uuid::new_v4(),
            name: name.to_string(),
            profile_id,
            source: SavedChartSource::Query {
                query: "SELECT 1".to_string(),
            },
            chart_spec,
            bindings: Default::default(),
            time_range_preset: None,
            refresh_policy: SavedChartRefreshPolicy::Off,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    // ----- existing-style tests -----

    #[test]
    fn submit_label_updates_live_based_on_selection_count() {
        let ids_0: Vec<Uuid> = vec![];
        let ids_1 = [Uuid::new_v4()];
        let ids_3 = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];

        assert_eq!(
            submit_label_for(AddPanelTab::Saved, ids_0.len()),
            datatab_i18n::t!("modals.add_panel_picker.submit.zero")
        );
        assert_eq!(
            submit_label_for(AddPanelTab::Saved, ids_1.len()),
            datatab_i18n::t!("modals.add_panel_picker.submit.one")
        );
        assert_eq!(
            submit_label_for(AddPanelTab::Saved, ids_3.len()),
            datatab_i18n::t!("modals.add_panel_picker.submit.many", count = ids_3.len())
        );
    }

    #[test]
    fn modal_add_panel_picker_filter_is_case_insensitive() {
        let profile_id = test_uuid();
        let candidates = vec![
            make_chart("Foo metric", profile_id),
            make_chart("FOO dashboard", profile_id),
            make_chart("bar chart", profile_id),
        ];

        let filtered = ModalAddPanelPicker::filtered_candidates(&candidates, "foo");
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|c| c.name == "Foo metric"));
        assert!(filtered.iter().any(|c| c.name == "FOO dashboard"));
    }

    // ----- new tests for multi-tab behaviour (pure helpers — no GPUI required) -----

    #[test]
    fn submit_label_per_tab_pure() {
        let zero = datatab_i18n::t!("modals.add_panel_picker.submit.zero");
        let one = datatab_i18n::t!("modals.add_panel_picker.submit.one");
        let many_5 = datatab_i18n::t!("modals.add_panel_picker.submit.many", count = 5);
        let create = datatab_i18n::t!("modals.add_panel_picker.submit.create");

        assert_eq!(submit_label_for(AddPanelTab::Saved, 0), zero);
        assert_eq!(submit_label_for(AddPanelTab::Saved, 1), one);
        assert_eq!(submit_label_for(AddPanelTab::Saved, 5), many_5);
        assert_eq!(submit_label_for(AddPanelTab::Query, 0), create);
        assert_eq!(submit_label_for(AddPanelTab::Query, 99), create);
    }

    #[test]
    fn query_tab_rejects_empty_name_or_query() {
        assert!(!query_tab_valid("", ""), "empty must be invalid");
        assert!(!query_tab_valid("Panel A", ""), "name only is not enough");
        assert!(!query_tab_valid("", "SELECT 1"), "query only is not enough");
        assert!(
            query_tab_valid("Panel A", "SELECT 1"),
            "name + query must be valid"
        );
        assert!(
            !query_tab_valid("   ", "SELECT 1"),
            "whitespace-only name must be invalid"
        );
        assert!(
            !query_tab_valid("Panel A", "  \n  "),
            "whitespace-only query must be invalid"
        );
    }

    #[test]
    fn outcome_variants_are_distinct() {
        // Pins the AddPanelOutcome enum shape so future refactors don't
        // silently collapse the two paths back into one.
        let dashboard_id = test_uuid();
        let profile_id = Uuid::new_v4();

        let confirmed = AddPanelOutcome::Confirmed {
            dashboard_id,
            chart_ids: vec![Uuid::new_v4()],
        };
        let from_query = AddPanelOutcome::CreateFromQuery {
            dashboard_id,
            profile_id,
            name: "Panel A".to_string(),
            query: "SELECT 1".to_string(),
            chart_kind: ChartKind::Bar,
        };
        let cancelled = AddPanelOutcome::Cancelled;

        // Match exhaustively so adding a future variant forces a compile error.
        for outcome in [&confirmed, &from_query, &cancelled] {
            match outcome {
                AddPanelOutcome::Confirmed { .. }
                | AddPanelOutcome::CreateFromQuery { .. }
                | AddPanelOutcome::Cancelled => {}
            }
        }

        // Field round-trip on CreateFromQuery.
        if let AddPanelOutcome::CreateFromQuery {
            dashboard_id: d,
            profile_id: p,
            name,
            query,
            chart_kind,
        } = &from_query
        {
            assert_eq!(*d, dashboard_id);
            assert_eq!(*p, profile_id);
            assert_eq!(name, "Panel A");
            assert_eq!(query, "SELECT 1");
            assert_eq!(*chart_kind, ChartKind::Bar);
        } else {
            panic!("from_query did not match CreateFromQuery");
        }
    }

    // ----- i18n catalog coverage -----

    const ADD_PANEL_PICKER_KEYS: &[&str] = &[
        "modals.add_panel_picker.title",
        "modals.add_panel_picker.submit.zero",
        "modals.add_panel_picker.submit.one",
        "modals.add_panel_picker.submit.many",
        "modals.add_panel_picker.submit.create",
        "modals.add_panel_picker.saved.empty",
        "modals.add_panel_picker.saved.search_label",
        "modals.add_panel_picker.saved.search_placeholder",
        "modals.add_panel_picker.tab.saved",
        "modals.add_panel_picker.tab.query",
        "modals.add_panel_picker.name_placeholder",
        "modals.add_panel_picker.name_label",
        "modals.add_panel_picker.query.placeholder",
        "modals.add_panel_picker.query.label",
        "modals.add_panel_picker.chart_kind.label",
        "modals.add_panel_picker.chart_kind.line",
        "modals.add_panel_picker.chart_kind.bar",
        "modals.add_panel_picker.chart_kind.area",
        "modals.add_panel_picker.chart_kind.scatter",
        "modals.add_panel_picker.cancel",
    ];

    #[test]
    fn add_panel_picker_catalog_keys_resolve() {
        for key in ADD_PANEL_PICKER_KEYS {
            let en = datatab_i18n::t!(key);
            assert!(!en.is_empty(), "empty English translation for {key}");
            assert_ne!(en, *key, "missing English translation for {key}");
        }
    }

    #[test]
    fn chart_kind_labels_resolve() {
        for kind in [
            ChartKind::Line,
            ChartKind::Bar,
            ChartKind::Area,
            ChartKind::Scatter,
        ] {
            let key = chart_kind_label_key(kind);
            let en = datatab_i18n::t!(key);
            assert!(!en.is_empty());
        }
    }

    #[test]
    fn submit_label_for_zero_uses_zero_bucket() {
        assert_eq!(
            submit_label_for(AddPanelTab::Saved, 0),
            datatab_i18n::t!("modals.add_panel_picker.submit.zero")
        );
    }

    #[test]
    fn submit_label_for_one_uses_one_bucket() {
        assert_eq!(
            submit_label_for(AddPanelTab::Saved, 1),
            datatab_i18n::t!("modals.add_panel_picker.submit.one")
        );
    }

    #[test]
    fn submit_label_for_many_interpolates_count() {
        let label = submit_label_for(AddPanelTab::Saved, 5);
        assert_eq!(
            label,
            datatab_i18n::t!("modals.add_panel_picker.submit.many", count = 5)
        );
        assert!(label.contains('5'));
    }
}

#[cfg(test)]
mod keyboard_tests {
    // Explicit imports rather than the parent glob: combining `use super::*`
    // with `#[gpui::test]` sends the gpui_macros expansion into unbounded
    // recursion.
    use super::{AddPanelOutcome, AddPanelRequest, AddPanelTab, ModalAddPanelPicker};
    use crate::modals::test_host::{click_backdrop, has_focus, host_modal};
    use datatab_components::chart::ChartSpec;
    use datatab_components::saved_chart::{SavedChart, SavedChartRefreshPolicy, SavedChartSource};
    use gpui::{Entity, FocusHandle, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;
    use uuid::Uuid;

    type Outcomes = Rc<RefCell<Vec<AddPanelOutcome>>>;

    fn chart() -> SavedChart {
        let chart_spec: ChartSpec = serde_json::from_str(
            r#"{"x_axis":{"column_index":0,"label":"t","kind":"Time","unit":null},"series":[]}"#,
        )
        .expect("valid chart spec");

        SavedChart {
            id: Uuid::nil(),
            name: "Latency".to_string(),
            profile_id: Uuid::nil(),
            source: SavedChartSource::Query {
                query: "SELECT 1".to_string(),
            },
            chart_spec,
            bindings: Default::default(),
            time_range_preset: None,
            refresh_policy: SavedChartRefreshPolicy::Off,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn open_modal(
        cx: &mut TestAppContext,
    ) -> (
        Entity<ModalAddPanelPicker>,
        FocusHandle,
        &mut VisualTestContext,
        Outcomes,
    ) {
        let (modal, outside, window) = host_modal(cx, ModalAddPanelPicker::new);

        let outcomes: Outcomes = Rc::default();
        window.update(|window, cx| {
            let sink = outcomes.clone();
            cx.subscribe(&modal, move |_, outcome: &AddPanelOutcome, _| {
                sink.borrow_mut().push(outcome.clone());
            })
            .detach();

            modal.update(cx, |modal, cx| {
                modal.open(
                    AddPanelRequest {
                        dashboard_id: Uuid::nil(),
                        profile_id: Uuid::nil(),
                        candidates: vec![chart()],
                    },
                    window,
                    cx,
                );
            });
        });
        window.run_until_parked();

        (modal, outside, window, outcomes)
    }

    /// Enter adds the checked charts, or with none checked the highlighted
    /// one; with no chart left in the list it does nothing.
    #[gpui::test]
    fn enter_adds_the_highlighted_chart_when_none_is_checked(cx: &mut TestAppContext) {
        let (modal, _outside, window, outcomes) = open_modal(cx);
        assert_eq!(
            window.update(|_, cx| modal.read(cx).active_tab()),
            AddPanelTab::Saved
        );

        window.simulate_keystrokes("x enter");
        assert!(
            outcomes.borrow().is_empty(),
            "a search that leaves no chart adds nothing"
        );
        assert!(window.update(|_, cx| modal.read(cx).is_visible()));

        window.simulate_keystrokes("backspace enter");

        assert!(matches!(
            outcomes.borrow().as_slice(),
            [AddPanelOutcome::Confirmed { chart_ids, .. }] if chart_ids == &[Uuid::nil()]
        ));
        assert!(!window.update(|_, cx| modal.read(cx).is_visible()));
    }

    /// The same picker with custom saved charts.
    fn open_modal_with(
        cx: &mut TestAppContext,
        candidates: Vec<SavedChart>,
    ) -> (
        Entity<ModalAddPanelPicker>,
        &mut VisualTestContext,
        Outcomes,
    ) {
        let (modal, _outside, window) = host_modal(cx, ModalAddPanelPicker::new);

        let outcomes: Outcomes = Rc::default();
        window.update(|window, cx| {
            let sink = outcomes.clone();
            cx.subscribe(&modal, move |_, outcome: &AddPanelOutcome, _| {
                sink.borrow_mut().push(outcome.clone());
            })
            .detach();

            modal.update(cx, |modal, cx| {
                modal.open(
                    AddPanelRequest {
                        dashboard_id: Uuid::nil(),
                        profile_id: Uuid::nil(),
                        candidates,
                    },
                    window,
                    cx,
                );
            });
        });
        window.run_until_parked();

        (modal, window, outcomes)
    }

    fn named_chart(name: &str) -> SavedChart {
        SavedChart {
            id: Uuid::new_v4(),
            name: name.to_string(),
            ..chart()
        }
    }

    /// Alt+L and Alt+H switch the tabs from the search field, wrapping.
    #[gpui::test]
    fn alt_l_and_alt_h_switch_the_tabs(cx: &mut TestAppContext) {
        let (modal, window, _outcomes) = open_modal_with(cx, vec![chart()]);
        let tab =
            |window: &mut VisualTestContext| window.update(|_, cx| modal.read(cx).active_tab());

        window.simulate_keystrokes("alt-l");
        assert_eq!(tab(window), AddPanelTab::Query);

        window.simulate_keystrokes("alt-l");
        assert_eq!(tab(window), AddPanelTab::Saved, "the tabs wrap around");

        window.simulate_keystrokes("alt-h");
        assert_eq!(tab(window), AddPanelTab::Query);
    }

    /// From the search field Down moves the highlighted chart and Enter adds
    /// it when nothing is checked.
    #[gpui::test]
    fn down_and_enter_add_the_highlighted_chart(cx: &mut TestAppContext) {
        let first = named_chart("Latency");
        let second = named_chart("Throughput");
        let second_id = second.id;
        let (_modal, window, outcomes) = open_modal_with(cx, vec![first, second]);

        window.simulate_keystrokes("down enter");

        assert!(matches!(
            outcomes.borrow().as_slice(),
            [AddPanelOutcome::Confirmed { chart_ids, .. }] if chart_ids == &[second_id]
        ));
    }

    /// In the chart list (the tab stop after the search) J moves and Space
    /// checks charts; Enter adds the checked ones.
    #[gpui::test]
    fn j_and_space_check_charts_in_the_list(cx: &mut TestAppContext) {
        let first = named_chart("Latency");
        let second = named_chart("Throughput");
        let ids = [first.id, second.id];
        let (modal, window, outcomes) = open_modal_with(cx, vec![first, second]);

        window.update(|window, cx| {
            let list = modal.read(cx).saved_list_focus.clone();
            list.focus(window, cx);
        });
        window.run_until_parked();

        window.simulate_keystrokes("space j space");
        assert_eq!(
            window.update(|_, cx| modal.read(cx).selected_ids.clone()),
            ids.to_vec()
        );

        window.simulate_keystrokes("enter");
        assert!(matches!(
            outcomes.borrow().as_slice(),
            [AddPanelOutcome::Confirmed { chart_ids, .. }] if chart_ids == &ids.to_vec()
        ));
    }

    #[gpui::test]
    fn escape_cancels_and_gives_focus_back(cx: &mut TestAppContext) {
        let (modal, outside, window, outcomes) = open_modal(cx);

        window.simulate_keystrokes("escape");

        assert!(matches!(
            outcomes.borrow().as_slice(),
            [AddPanelOutcome::Cancelled]
        ));
        assert!(!window.update(|_, cx| modal.read(cx).is_visible()));
        assert!(has_focus(window, &outside));
    }

    #[gpui::test]
    fn a_backdrop_click_cancels(cx: &mut TestAppContext) {
        let (_modal, _outside, window, outcomes) = open_modal(cx);

        click_backdrop(window);

        assert!(matches!(
            outcomes.borrow().as_slice(),
            [AddPanelOutcome::Cancelled]
        ));
    }

    /// With Vim mode on, the query editor keeps its keys: `j` moves down,
    /// the first Escape leaves Insert mode, Enter in Normal mode never
    /// submits, and Escape in Normal mode cancels the dialog.
    #[gpui::test]
    fn the_query_editor_takes_vim_keys(cx: &mut TestAppContext) {
        cx.update(|cx| datatab_components::vim::set_vim_enabled(cx, true));
        let (modal, _outside, window, outcomes) = open_modal(cx);

        window.update(|window, cx| {
            modal.update(cx, |modal, cx| {
                modal.active_tab = AddPanelTab::Query;
                modal.query_name_input.update(cx, |state, cx| {
                    state.set_value("Latency", window, cx);
                });
                modal.query_input.update(cx, |state, cx| {
                    state.set_value("SELECT 1\nFROM t", window, cx);
                    state.set_selected_range(0..0, cx);
                    state.focus(window, cx);
                });
                cx.notify();
            });
        });
        window.run_until_parked();

        let text_and_cursor = |window: &mut VisualTestContext| {
            window.update(|_, cx| {
                let state = modal.read(cx).query_input.read(cx);
                (state.value().to_string(), state.cursor())
            })
        };

        window.simulate_keystrokes("j");
        window.run_until_parked();
        assert_eq!(text_and_cursor(window), ("SELECT 1\nFROM t".into(), 9));

        window.simulate_keystrokes("enter");
        window.run_until_parked();
        assert!(
            outcomes.borrow().is_empty(),
            "Enter in Normal mode submitted"
        );

        window.simulate_keystrokes("i");
        window.simulate_input("X");
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        assert_eq!(text_and_cursor(window).0, "SELECT 1\nXFROM t");
        assert!(
            window.update(|_, cx| modal.read(cx).is_visible()),
            "the first Escape only leaves Insert mode"
        );

        window.simulate_keystrokes("escape");
        window.run_until_parked();
        assert!(!window.update(|_, cx| modal.read(cx).is_visible()));
    }

    use crate::keyboard_coverage::KeyboardPath;
    use datatab_app::keymap::Command;

    /// The Add Panel dialog's registry (see `crate::keyboard_coverage`).
    const ADD_PANEL_PICKER: crate::keyboard_coverage::SurfaceRegistry =
        crate::keyboard_coverage::SurfaceRegistry {
            name: "Add Panel dialog",
            contexts: &[
                datatab_app::keymap::ContextId::AddPanelPicker,
                datatab_app::keymap::ContextId::Modal,
            ],
            entries: &[
                ("add-panel-cancel", KeyboardPath::Command(Command::Cancel)),
                ("add-panel-confirm", KeyboardPath::Command(Command::Execute)),
                (
                    "add-panel-tab-*",
                    KeyboardPath::Command(Command::NextPanelTab),
                ),
                // Tab moves into the chart list, J and K move, Space checks.
                (
                    "add-panel-chart-row-*",
                    KeyboardPath::Command(Command::ExpandCollapse),
                ),
                ("add-panel-kind-*", KeyboardPath::TabStop),
            ],
        };

    /// Every tab of the Add Panel dialog. The keyboard tests above prove
    /// its keys; the dashboard's A (or its Add Panel button) opens it.
    #[gpui::test]
    fn the_add_panel_dialog_is_covered(cx: &mut TestAppContext) {
        use crate::keyboard_coverage::{Coverage, FrameCapture, MODAL_CHROME};

        cx.update(crate::keymap::init_keymap);
        let (_modal, window, _outcomes) =
            open_modal_with(cx, vec![named_chart("Latency"), named_chart("Errors")]);
        let capture = FrameCapture::observe(window);
        let coverage = || Coverage::new(ADD_PANEL_PICKER).with_surface(MODAL_CHROME);

        let mut checked = Vec::new();
        for _tab in 0..2 {
            checked.extend(coverage().assert_covered(&capture.frame(window)));
            window.simulate_keystrokes("alt-l");
            window.run_until_parked();
        }

        for expected in ["add-panel-chart-row-", "add-panel-kind-"] {
            assert!(
                checked.iter().any(|id| id.starts_with(expected)),
                "{expected} in {checked:?}"
            );
        }
    }
}
