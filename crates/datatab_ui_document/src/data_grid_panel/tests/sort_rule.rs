//! The grid's sort rule for a driver that cannot sort (ruling 4 on #64).
//!
//! A table whose driver lacks `DriverCapabilities::SORTING` sends no server
//! `order_by`, and its header offers no sort: a locally sorted page would claim
//! an order the rest of the table does not have, and a page walked through the
//! server's paging state would carry the next page in the table's own order.

use super::*;

/// A table panel over `users` with `id` as its key, as `new_for_table` builds
/// one once its first page is in.
fn table_panel_with(
    cx: &mut TestAppContext,
    connection: Arc<dyn datatab_core::Connection>,
) -> (
    gpui::Entity<AppStateEntity>,
    Uuid,
    gpui::Entity<DataGridPanel>,
    &mut VisualTestContext,
) {
    let (app_state, profile_id) = register_stub_connection(cx, connection);
    let slot: Rc<RefCell<Option<gpui::Entity<DataGridPanel>>>> = Rc::default();
    let slot_for_window = slot.clone();
    let app_state_for_panel = app_state.clone();

    let (_, window) = cx.add_window_view(move |window, cx| {
        let panel = cx.new(|cx| {
            let source = DataSource::Table {
                profile_id,
                database: Some("app".to_string()),
                table: TableRef::with_schema("public", "users"),
                pagination: Pagination::default(),
                order_by: Vec::new(),
                total_rows: None,
            };
            DataGridPanel::new_internal(
                source,
                app_state_for_panel.clone(),
                vec!["id".to_string()],
                window,
                cx,
            )
        });
        slot_for_window.replace(Some(panel.clone()));
        Root::new(panel, window, cx)
    });
    window.run_until_parked();

    let panel = slot.borrow().clone().expect("the window built the grid");
    window.update(|_, cx| {
        panel.update(cx, |panel, cx| {
            panel.set_result(reload_result(&["id", "name"], 3), cx);
        });
    });
    window.run_until_parked();

    (app_state, profile_id, panel, window)
}

#[gpui::test]
fn a_driver_that_cannot_sort_blocks_the_header_and_sends_no_order(cx: &mut TestAppContext) {
    let (_, _profile_id, panel, window) = table_panel_with(cx, Arc::new(StubConnection::new()));

    window.update(|_, cx| {
        let table_state = panel
            .read(cx)
            .grid_table
            .table_state
            .clone()
            .expect("the grid built a table");

        assert!(
            panel.read(cx).table_sort_unavailable(cx).is_some(),
            "the grid says why the header cannot sort"
        );
        let reason = table_state
            .read(cx)
            .sort_blocked()
            .cloned()
            .expect("the header carries the reason");
        assert!(
            reason.contains("cannot sort"),
            "the reason names the limitation: {reason}"
        );

        panel.update(cx, |panel, cx| {
            panel.handle_sort_request(1, datatab_core::SortDirection::Ascending, cx);
            assert!(
                panel.pending.requery.is_none(),
                "no server order is asked for"
            );
            panel.handle_sort_clear(cx);
            assert!(
                panel.pending.requery.is_none(),
                "clearing an order asks for none either"
            );
        });
    });
}

#[gpui::test]
fn a_driver_that_sorts_keeps_its_header_and_asks_the_server(cx: &mut TestAppContext) {
    let (_, _profile_id, panel, window) = table_panel_with(cx, Arc::new(StubConnection::sorting()));

    window.update(|_, cx| {
        let table_state = panel
            .read(cx)
            .grid_table
            .table_state
            .clone()
            .expect("the grid built a table");

        assert!(panel.read(cx).table_sort_unavailable(cx).is_none());
        assert!(
            table_state.read(cx).sort_blocked().is_none(),
            "a sorting driver's header is untouched"
        );

        panel.update(cx, |panel, cx| {
            panel.handle_sort_request(1, datatab_core::SortDirection::Descending, cx);
            let requery = panel
                .pending
                .requery
                .as_ref()
                .expect("a sorting driver re-queries with the order");
            assert_eq!(requery.order_by.len(), 1);
            assert_eq!(requery.order_by[0].column.name, "name");
        });
    });
}

#[gpui::test]
fn a_query_result_keeps_its_local_sort(cx: &mut TestAppContext) {
    init_test_runtime(cx);
    let app_state = isolated_test_app_state(cx);
    let (_, panel, window) = host_result_grid(cx, app_state);

    window.update(|_, cx| {
        assert!(
            panel.read(cx).table_sort_unavailable(cx).is_none(),
            "a result has no table behind it to ask for an order"
        );
    });
}

#[gpui::test]
fn the_browse_order_is_the_key_only_for_a_driver_that_sorts(cx: &mut TestAppContext) {
    let (sorting_state, sorting_id) =
        register_stub_connection(cx, Arc::new(StubConnection::sorting()));
    let (plain_state, plain_id) = register_stub_connection(cx, Arc::new(StubConnection::new()));
    let key_order = vec![datatab_core::OrderByColumn::asc("id")];

    cx.update(|cx| {
        assert_eq!(
            DataGridPanel::browse_order(&sorting_state, sorting_id, key_order.clone(), cx),
            key_order,
            "a driver that sorts keeps the key order it was given"
        );
        assert!(
            DataGridPanel::browse_order(&plain_state, plain_id, key_order, cx).is_empty(),
            "a driver that cannot sort is sent no order"
        );
    });
}

// =============================================================================
// The refused sort reaches assistive technology, and the key columns survive
// =============================================================================

/// The column-header node whose accessible name is `name`.
fn header_node<'a>(frame: &'a gpui::AccessibilityFrame, name: &str) -> &'a gpui::accesskit::Node {
    frame
        .nodes()
        .filter_map(|(_, node)| frame.accessibility_node(node))
        .find(|node| node.role() == gpui::Role::ColumnHeader && node.label() == Some(name))
        .unwrap_or_else(|| panic!("a column header named {name} is in the tree"))
}

fn draw(window: &mut VisualTestContext) {
    window.update(|window, _| window.refresh());
    window.run_until_parked();
}

#[gpui::test]
fn a_refused_sort_is_a_disabled_column_header_with_the_reason(cx: &mut TestAppContext) {
    let (_, _profile_id, _panel, window) = table_panel_with(cx, Arc::new(StubConnection::new()));
    let capture = datatab_components::accessibility::FrameCapture::observe(window);
    draw(window);

    let frame = capture.frame(window);
    let header = header_node(&frame, "name");

    assert!(
        header.is_disabled(),
        "a header that cannot sort is announced as disabled"
    );
    assert_eq!(
        header.description(),
        Some(crate::labels::sort_unavailable().as_str()),
        "the reason reaches assistive technology, not only the tooltip"
    );
}

#[gpui::test]
fn a_sortable_header_is_an_enabled_named_column_header(cx: &mut TestAppContext) {
    let (_, _profile_id, _panel, window) =
        table_panel_with(cx, Arc::new(StubConnection::sorting()));
    let capture = datatab_components::accessibility::FrameCapture::observe(window);
    draw(window);

    let frame = capture.frame(window);
    let header = header_node(&frame, "name");

    assert!(!header.is_disabled(), "a sorting header stays actionable");
    assert_eq!(
        header.description(),
        None,
        "a sorting header carries no refusal"
    );
}

#[gpui::test]
fn the_refused_ordering_rows_carry_the_reason(cx: &mut TestAppContext) {
    use datatab_components::components::data_table::DataTableEvent;

    let (_, _profile_id, panel, window) = table_panel_with(cx, Arc::new(StubConnection::new()));
    window.update(|_, cx| {
        let table_state = panel
            .read(cx)
            .grid_table
            .table_state
            .clone()
            .expect("the grid built a table");
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

    let capture = datatab_components::accessibility::FrameCapture::observe(window);
    draw(window);
    let frame = capture.frame(window);
    let reason = crate::labels::sort_unavailable();

    // Row 0 is "Copy column name"; rows 1 to 3 are the ordering rows.
    let copy =
        datatab_components::accessibility::accessibility_node(&frame, "column-menu-action-0")
            .expect("the copy row is in the menu");
    assert!(!copy.is_disabled(), "copying a name still works");

    for index in 1..=3 {
        let id = format!("column-menu-action-{index}");
        let row = datatab_components::accessibility::accessibility_node(&frame, &id)
            .unwrap_or_else(|| panic!("{id} is in the menu"));
        assert!(row.is_disabled(), "{id} is offered as disabled");
        let name = datatab_components::accessibility::accessible_name(&frame, &id)
            .unwrap_or_else(|| panic!("{id} has a name"));
        assert!(
            name.contains(&reason),
            "{id} carries the reason in its own text: {name}"
        );
    }
}

#[gpui::test]
fn a_table_without_a_sort_opens_with_its_key_columns(cx: &mut TestAppContext) {
    use datatab_core::{ColumnInfo, DbSchemaInfo, RelationalSchema, SchemaSnapshot, TableInfo};

    let (app_state, profile_id) = register_stub_connection(cx, Arc::new(StubConnection::new()));
    let users = TableRef::with_schema("app", "users");
    let columns = vec![
        ColumnInfo {
            name: "id".to_string(),
            type_name: "int".to_string(),
            nullable: false,
            is_primary_key: true,
            default_value: None,
            enum_values: None,
        },
        ColumnInfo {
            name: "name".to_string(),
            type_name: "text".to_string(),
            nullable: true,
            is_primary_key: false,
            default_value: None,
            enum_values: None,
        },
    ];
    let table = TableInfo {
        name: "users".to_string(),
        schema: Some("app".to_string()),
        columns: Some(columns),
        indexes: None,
        foreign_keys: None,
        constraints: None,
        sample_fields: None,
        presentation: datatab_core::CollectionPresentation::DataGrid,
        storage_hints: None,
        child_items: None,
        pseudo_columns: Box::default(),
    };

    // The key arrives with the schema the sidebar loaded, not with the table's
    // details, so nothing repairs the panel after `new_for_table` returns: the
    // key columns it opens with are the ones the constructor worked out.
    cx.update(|cx| {
        app_state.update(cx, |app, _| {
            let connected = app
                .connections_mut()
                .get_mut(&profile_id)
                .expect("the stub profile is registered");
            connected.schema = Some(SchemaSnapshot::relational(RelationalSchema {
                databases: Vec::new(),
                current_database: Some("app".to_string()),
                schemas: vec![DbSchemaInfo {
                    name: "app".to_string(),
                    tables: vec![table],
                    views: Vec::new(),
                    custom_types: None,
                }],
                tables: Vec::new(),
                views: Vec::new(),
            }));
        });
    });

    let slot: Rc<RefCell<Option<gpui::Entity<DataGridPanel>>>> = Rc::default();
    let slot_for_window = slot.clone();
    let app_state_for_panel = app_state.clone();
    let (_, window) = cx.add_window_view(move |window, cx| {
        let panel = cx.new(|cx| {
            DataGridPanel::new_for_table(
                profile_id,
                users.clone(),
                Some("app".to_string()),
                app_state_for_panel.clone(),
                window,
                cx,
            )
        });
        slot_for_window.replace(Some(panel.clone()));
        Root::new(panel, window, cx)
    });
    window.run_until_parked();

    let panel = slot.borrow().clone().expect("the window built the grid");
    window.update(|_, app| {
        let panel = panel.read(app);
        assert_eq!(
            panel.pk_columns,
            vec!["id".to_string()],
            "the key comes from the table's key columns, not from the sort order"
        );
        assert!(
            panel.mutations_enabled(),
            "the grid opens editable instead of waiting for a details fetch"
        );
        match &panel.source {
            DataSource::Table { order_by, .. } => assert!(
                order_by.is_empty(),
                "a driver that cannot sort is sent no order"
            ),
            _ => panic!("a table source"),
        }
    });
}
