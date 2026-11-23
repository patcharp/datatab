use super::*;

use datatab_components::accessibility::{FrameCapture, accessibility_node, accessible_name};
use gpui::{Context, Role, VisualTestContext, Window};

/// The catalog sentence the free-form banner shows.
fn free_form_hint() -> String {
    datatab_i18n::t!("document.data.grid.editing.free_form_read_only").to_string()
}

fn query_result() -> QueryResult {
    QueryResult::table(
        vec![key_column("id", true), key_column("name", false)],
        vec![
            vec![
                datatab_core::Value::Int(1),
                datatab_core::Value::Text("ann".into()),
            ],
            vec![
                datatab_core::Value::Int(2),
                datatab_core::Value::Text("bob".into()),
            ],
        ],
        None,
        Duration::ZERO,
    )
}

fn table_source() -> DataSource {
    DataSource::Table {
        profile_id: Uuid::nil(),
        database: Some("app".to_string()),
        table: TableRef::with_schema("public", "users"),
        pagination: Pagination::default(),
        order_by: Vec::new(),
        total_rows: Some(1),
    }
}

/// Renders a panel as the window's root view, the way a document shows it, and
/// returns it with the window that drew it.
fn rendered(
    cx: &mut TestAppContext,
    build: impl FnOnce(
        gpui::Entity<AppStateEntity>,
        &mut Window,
        &mut Context<DataGridPanel>,
    ) -> DataGridPanel,
) -> (gpui::Entity<DataGridPanel>, &mut VisualTestContext) {
    init_test_runtime(cx);
    let app_state = isolated_test_app_state(cx);
    let holder = Rc::new(RefCell::new(None));
    let panel_slot = holder.clone();

    let (_, window) = cx.add_window_view(move |window, cx| {
        let panel = cx.new(|cx| build(app_state, window, cx));
        panel_slot.replace(Some(panel.clone()));
        Root::new(panel, window, cx)
    });
    window.run_until_parked();

    let panel = holder.borrow().clone().expect("the panel is created");
    (panel, window)
}

/// A free-form query result as the editor builds it: `new_for_result`, then the
/// mark that says the statement came from the editor.
fn free_form_panel(
    cx: &mut TestAppContext,
) -> (gpui::Entity<DataGridPanel>, &mut VisualTestContext) {
    rendered(cx, move |app_state, window, cx| {
        let mut panel = DataGridPanel::new_for_result(
            Arc::new(query_result()),
            "SELECT id, name FROM users".to_string(),
            None,
            app_state,
            window,
            cx,
        );
        panel.mark_free_form_query_result(cx);
        panel
    })
}

/// How many times `text` is drawn on the surface: the occurrences in the text
/// of the window root, which gathers every caption. A plain caption is not an
/// accessibility node of its own, so the gathered text is where a sentence
/// drawn twice shows twice.
fn times_drawn(frame: &gpui::AccessibilityFrame, text: &str) -> usize {
    frame
        .nodes()
        .find(|(_, node)| node.id() == "root")
        .map(|(_, node)| node.content_text().matches(text).count())
        .unwrap_or(0)
}

/// How many accessibility nodes expose `text`, as their own name or as the text
/// they draw. One means it is read once, not once as a name and again as text.
fn exposures_of(frame: &gpui::AccessibilityFrame, text: &str) -> usize {
    frame
        .nodes()
        .filter(|(_, node)| {
            let accessible = frame.accessibility_node(node);
            if accessible.is_some_and(|accessible| accessible.is_hidden()) {
                return false;
            }

            let named = accessible
                .and_then(|accessible| accessible.label())
                .is_some_and(|label| label.trim() == text);

            named || node.content_text().trim() == text
        })
        .count()
}

/// A free-form result says why it is read-only above its grid, and keeps the
/// footer's read-only note.
#[gpui::test]
fn a_free_form_result_explains_why_it_is_read_only(cx: &mut TestAppContext) {
    let (_panel, window) = free_form_panel(cx);
    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);

    assert_eq!(
        accessible_name(&frame, "free-form-readonly-banner"),
        Some(free_form_hint()),
        "the banner announces the reason"
    );
    let node = accessibility_node(&frame, "free-form-readonly-banner")
        .expect("the banner is an accessibility node");
    assert_eq!(node.role(), Role::Note, "the banner is read, not clicked");
    assert_eq!(
        times_drawn(&frame, &free_form_hint()),
        1,
        "the reason is drawn once"
    );
    assert_eq!(
        exposures_of(&frame, &free_form_hint()),
        1,
        "the sentence is read once, not as a name and again as its own text"
    );
    assert!(
        window.debug_bounds("footer-read-only").is_some(),
        "the footer keeps its read-only note"
    );
}

/// A result without the mark gets no explanation, whatever its source: the
/// instance inspector's metric grids come through `new_for_result` too, and
/// the visual query builder has nothing to do with them.
#[gpui::test]
fn a_result_no_host_marked_explains_nothing(cx: &mut TestAppContext) {
    let (_panel, window) = rendered(cx, move |app_state, window, cx| {
        DataGridPanel::new_for_result(
            Arc::new(query_result()),
            "vector_search_index_size_bytes".to_string(),
            None,
            app_state,
            window,
            cx,
        )
    });
    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);

    assert!(
        accessibility_node(&frame, "free-form-readonly-banner").is_none(),
        "an unmarked result is not the editor's free-form query"
    );
    assert_eq!(
        times_drawn(&frame, &free_form_hint()),
        0,
        "the free-form reason is not drawn"
    );
    // It is still a result nobody can edit, so the footer says so.
    assert!(window.debug_bounds("footer-read-only").is_some());
}

/// An editable table shows no read-only explanation: nothing is disabled.
#[gpui::test]
fn an_editable_table_shows_no_read_only_banner(cx: &mut TestAppContext) {
    let (panel, window) = rendered(cx, move |app_state, window, cx| {
        let mut panel = DataGridPanel::new_internal(
            table_source(),
            app_state,
            vec!["id".to_string()],
            window,
            cx,
        );
        panel.set_result(query_result(), cx);
        panel
    });
    let editable = window.update(|_, cx| {
        panel
            .read(cx)
            .grid_table
            .table_state
            .as_ref()
            .expect("a table")
            .read(cx)
            .is_editable()
    });
    assert!(
        editable,
        "the source has to be editable for the test to mean anything"
    );

    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);
    assert!(
        accessibility_node(&frame, "free-form-readonly-banner").is_none(),
        "an editable result explains nothing"
    );
    assert_eq!(times_drawn(&frame, &free_form_hint()), 0);
    assert!(window.debug_bounds("footer-read-only").is_none());
}

/// A result the host marked, whose grid is nevertheless editable, explains
/// nothing: there is no disabled cell to account for.
#[gpui::test]
fn a_marked_result_that_is_editable_shows_nothing(cx: &mut TestAppContext) {
    let (panel, window) = rendered(cx, move |app_state, window, cx| {
        let source = DataSource::QueryResult {
            result: Arc::new(query_result()),
            original_query: "SELECT id, name FROM users".to_string(),
            profile_id: None,
        };
        let mut panel =
            DataGridPanel::new_internal(source, app_state, vec!["id".to_string()], window, cx);
        panel.set_result(query_result(), cx);
        panel.mark_free_form_query_result(cx);
        panel
    });
    let editable = window.update(|_, cx| {
        panel
            .read(cx)
            .grid_table
            .table_state
            .as_ref()
            .expect("a table")
            .read(cx)
            .is_editable()
    });
    assert!(editable, "the key columns make this grid editable");

    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);
    assert!(accessibility_node(&frame, "free-form-readonly-banner").is_none());
    assert_eq!(times_drawn(&frame, &free_form_hint()), 0);
}

/// A view has no primary key, so its grid already says that. It never gets the
/// free-form explanation, which would name the wrong reason.
#[gpui::test]
fn a_view_keeps_the_missing_key_banner_only(cx: &mut TestAppContext) {
    let (_panel, window) = rendered(cx, move |app_state, window, cx| {
        let mut panel =
            DataGridPanel::new_internal(table_source(), app_state, Vec::new(), window, cx);
        panel.set_result(query_result(), cx);
        // No fetch is in flight, so the banner is not waiting for a key.
        panel.pk_details_pending = false;
        panel.pk_requery_pending = false;
        panel
    });
    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);

    assert!(
        window.debug_bounds("missing-primary-key-banner").is_some(),
        "a view is explained by its missing key"
    );
    assert!(
        accessibility_node(&frame, "free-form-readonly-banner").is_none(),
        "and not by words about a free-form result"
    );
    assert_eq!(times_drawn(&frame, &free_form_hint()), 0);
}

/// A read-only profile refuses the write, not the grid: the panel still holds
/// one table with its key, so it explains nothing here. The refusal shows when
/// a mutation runs, and the copy never promises an editable result.
#[gpui::test]
fn a_read_only_profile_shows_no_free_form_explanation(cx: &mut TestAppContext) {
    let (app_state, profile_id) = register_stub_connection(cx, Arc::new(StubConnection::new()));
    cx.update(|cx| {
        app_state.update(cx, |app, _| {
            app.connections_mut()
                .get_mut(&profile_id)
                .expect("the profile is connected")
                .mutation_policy = datatab_core::MutationPolicy::ReadOnly;
        });
    });
    let profile = profile_id;
    let holder = Rc::new(RefCell::new(None));
    let panel_slot = holder.clone();
    let (_, window) = cx.add_window_view(move |window, cx| {
        let panel = cx.new(|cx| {
            let mut panel = DataGridPanel::new_internal(
                DataSource::Table {
                    profile_id: profile,
                    database: Some("app".to_string()),
                    table: TableRef::with_schema("public", "users"),
                    pagination: Pagination::default(),
                    order_by: Vec::new(),
                    total_rows: Some(1),
                },
                app_state,
                vec!["id".to_string()],
                window,
                cx,
            );
            panel.set_result(query_result(), cx);
            panel
        });
        panel_slot.replace(Some(panel.clone()));
        Root::new(panel, window, cx)
    });
    window.run_until_parked();

    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);
    assert!(accessibility_node(&frame, "free-form-readonly-banner").is_none());
    assert_eq!(times_drawn(&frame, &free_form_hint()), 0);
}

/// A builder result keeps its own banner, drawn once, and gains no second one.
#[gpui::test]
fn a_builder_result_keeps_its_banner_once(cx: &mut TestAppContext) {
    let (_panel, window) = rendered(cx, move |app_state, window, cx| {
        let mut panel =
            DataGridPanel::new_internal(table_source(), app_state, Vec::new(), window, cx);
        // What `run_visual_query` leaves behind when no editable binding
        // could be built for the spec.
        panel.builder.current_visual_spec = Some(make_test_spec());
        panel.set_result(query_result(), cx);
        panel
    });
    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);
    let builder_text = datatab_i18n::t!("document.data.grid.editing.not_single_table").to_string();

    assert_eq!(
        times_drawn(&frame, &builder_text),
        1,
        "the builder's banner shows once"
    );
    assert!(
        accessibility_node(&frame, "free-form-readonly-banner").is_none(),
        "the free-form banner never joins it"
    );
}

/// A grouped result keeps its own banner and gains no second one.
#[gpui::test]
fn an_aggregate_result_keeps_its_own_banner(cx: &mut TestAppContext) {
    let (_panel, window) = rendered(cx, move |app_state, window, cx| {
        let mut panel =
            DataGridPanel::new_internal(table_source(), app_state, Vec::new(), window, cx);
        panel.builder.current_visual_spec = Some(make_grouped_spec());
        panel.set_result(query_result(), cx);
        panel
    });
    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);
    let aggregated = datatab_i18n::t!("document.data.grid.editing.aggregated").to_string();

    assert_eq!(
        times_drawn(&frame, &aggregated),
        1,
        "the aggregate banner stays, exactly once"
    );
    assert!(accessibility_node(&frame, "free-form-readonly-banner").is_none());
}

/// The free-form copy resolves through the catalog, not as a raw key.
#[test]
fn the_free_form_read_only_copy_resolves() {
    let english = datatab_i18n::t!(
        "document.data.grid.editing.free_form_read_only",
        locale = "en"
    );

    assert_eq!(
        english,
        "This result is not tied to one table and its primary key, so rows cannot be edited here.",
        "the catalog text is the copy the banner shows"
    );
}
