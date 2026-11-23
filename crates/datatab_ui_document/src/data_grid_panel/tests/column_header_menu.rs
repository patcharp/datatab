use super::*;
use crate::keyboard_test_support::init_keyboard_runtime;
use datatab_components::components::data_table::{ContextMenuAction, DataTableEvent};

#[gpui::test]
fn selecting_copy_column_name_copies_the_header_name(cx: &mut TestAppContext) {
    init_keyboard_runtime(cx);
    let app_state = isolated_test_app_state(cx);
    let (_host, panel, window) = host_result_grid(cx, app_state);
    window.update(|_, cx| {
        let table_state = panel
            .read(cx)
            .grid_table
            .table_state
            .clone()
            .expect("table");
        table_state.update(cx, |_, cx| {
            cx.emit(DataTableEvent::ContextMenuRequested {
                row: 0,
                col: 1,
                position: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                is_column_header: true,
            })
        });
    });
    window.run_until_parked();
    window.update(|window, cx| {
        panel.update(cx, |panel, cx| {
            assert!(panel.dispatch_command(Command::MenuSelect, window, cx));
        })
    });
    window.run_until_parked();
    let copied = window
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("name"));
    assert!(window.update(|_, cx| panel.read(cx).context_menu.is_none()));
}

#[test]
fn copy_column_name_heads_the_column_header_menu() {
    let actions = DataGridPanel::column_header_menu_fixed_actions();
    assert_eq!(
        actions,
        [
            ContextMenuAction::CopyColumnName,
            ContextMenuAction::Order(datatab_core::SortDirection::Ascending),
            ContextMenuAction::Order(datatab_core::SortDirection::Descending),
            ContextMenuAction::RemoveOrdering,
        ]
    );
}

#[gpui::test]
fn table_menu_copies_the_active_column_name_from_the_keyboard(cx: &mut TestAppContext) {
    use datatab_app::keymap::ContextId;
    init_keyboard_runtime(cx);
    let app_state = isolated_test_app_state(cx);
    let (_host, panel, window) = host_result_grid(cx, app_state);
    window.update(|_, cx| {
        let table_state = panel
            .read(cx)
            .grid_table
            .table_state
            .clone()
            .expect("table");
        table_state.update(cx, |state, cx| state.select_cell(CellCoord::new(0, 1), cx));
    });
    window.run_until_parked();
    window.simulate_keystrokes("m");
    window.run_until_parked();
    assert_eq!(
        window.update(|_, cx| panel.read(cx).active_context(cx)),
        ContextId::ContextMenu
    );
    for key in ["j", "enter"] {
        window.simulate_keystrokes(key);
        window.run_until_parked();
    }
    let copied = window
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("name"));
    assert!(window.update(|_, cx| panel.read(cx).context_menu.is_none()));
}
