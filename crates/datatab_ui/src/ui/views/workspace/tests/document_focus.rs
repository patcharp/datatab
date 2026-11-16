// Explicit imports, not `use super::*`: the parent glob together with
// `#[gpui::test]` sends the macro expansion into unbounded recursion.
use crate::ui::document::{DataDocument, Tab};
use crate::ui::views::workspace::{FocusTarget, Workspace};
use datatab_app::keymap::ContextId;
use datatab_ui_base::AppStateEntity;
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use std::sync::Arc;

/// A document connection that answers no query. It exists so a collection
/// tab renders its query bar in the workspace without a database.
struct StubDocumentConnection {
    metadata: datatab_core::DriverMetadata,
    features: datatab_core::DocumentFeatures,
}

impl StubDocumentConnection {
    fn new() -> Self {
        Self {
            metadata: datatab_core::DriverMetadata {
                id: "stub-documents".to_string(),
                display_name: "Stub".to_string(),
                description: "test stub".to_string(),
                category: datatab_core::DatabaseCategory::Document,
                transfer_family: datatab_core::TransferFamily::Incompatible,
                deployment_class: None,
                query_language: datatab_core::QueryLanguage::MongoQuery,
                capabilities: datatab_core::DriverCapabilities::empty(),
                default_port: None,
                uri_scheme: "stub".to_string(),
                icon: datatab_core::Icon::Database,
                syntax: None,
                query: None,
                mutation: None,
                ddl: None,
                transactions: None,
                limits: None,
                ssl_modes: None,
                ssl_cert_fields: None,
                classification_override: None,
                default_chunk_size: None,
                supports_lock_timeout: false,
                editor_profile: None,
            },
            features: datatab_core::DocumentFeatures::QUERY_SLOTS,
        }
    }
}

impl datatab_core::Connection for StubDocumentConnection {
    /// Facets as a driver of the stub's category declares them.
    fn collection_facets(
        &self,
        _collection: &datatab_core::CollectionRef,
    ) -> Vec<datatab_core::ObjectFacet> {
        datatab_core::category_collection_facets(self.metadata().category)
    }

    fn metadata(&self) -> &datatab_core::DriverMetadata {
        &self.metadata
    }

    fn kind(&self) -> datatab_core::DbKind {
        datatab_core::DbKind::MongoDB
    }

    fn schema_loading_strategy(&self) -> datatab_core::SchemaLoadingStrategy {
        datatab_core::SchemaLoadingStrategy::SingleDatabase
    }

    fn dialect(&self) -> &dyn datatab_core::SqlDialect {
        &datatab_core::DefaultSqlDialect
    }

    fn ping(&self) -> Result<(), datatab_core::DbError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), datatab_core::DbError> {
        Ok(())
    }

    fn execute(
        &self,
        _request: &datatab_core::QueryRequest,
    ) -> Result<datatab_core::QueryResult, datatab_core::DbError> {
        Err(datatab_core::DbError::NotSupported("stub".to_string()))
    }

    fn cancel(&self, _handle: &datatab_core::QueryHandle) -> Result<(), datatab_core::DbError> {
        Ok(())
    }

    fn schema(&self) -> Result<datatab_core::SchemaSnapshot, datatab_core::DbError> {
        Ok(datatab_core::SchemaSnapshot::default())
    }

    fn document_features(&self) -> datatab_core::DocumentFeatures {
        self.features
    }

    fn browse_collection(
        &self,
        _request: &datatab_core::CollectionBrowseRequest,
    ) -> Result<datatab_core::QueryResult, datatab_core::DbError> {
        Ok(datatab_core::QueryResult::json(
            Vec::new(),
            Vec::new(),
            std::time::Duration::from_millis(1),
        ))
    }
}

/// Registers the stub as the profile behind a collection tab.
fn register_stub_document_connection(
    cx: &mut TestAppContext,
) -> (Entity<AppStateEntity>, uuid::Uuid) {
    let profile_id = uuid::Uuid::new_v4();

    let app_state: Entity<AppStateEntity> = cx.update(|cx| {
        cx.new(|_| {
            let runtime =
                datatab_storage::bootstrap::StorageRuntime::in_memory().expect("in-memory storage");
            AppStateEntity::new_with_storage_runtime(runtime).expect("test storage setup")
        })
    });

    cx.update(|cx| {
        app_state.update(cx, |app, _cx| {
            let profile = datatab_core::ConnectionProfile::new(
                "test",
                datatab_core::DbConfig::SQLite {
                    path: std::path::PathBuf::from(":memory:"),
                    connection_id: None,
                },
            );
            let connected = datatab_core::ConnectedProfile {
                profile,
                connection: Arc::new(StubDocumentConnection::new()),
                schema: None,
                mutation_policy: datatab_core::MutationPolicy::default(),
                read_only_reason: None,
                database_schemas: Default::default(),
                table_details: Default::default(),
                schema_types: Default::default(),
                schema_columns: Default::default(),
                schema_indexes: Default::default(),
                schema_foreign_keys: Default::default(),
                schema_routines: Default::default(),
                dependents_cache: Default::default(),
                active_database: None,
                redis_key_cache: Default::default(),
                database_connections: Default::default(),
                proxy_tunnel: None,
            };
            app.connections_mut().insert(profile_id, connected);
        });
    });

    (app_state, profile_id)
}

fn new_document_workspace(
    cx: &mut TestAppContext,
) -> (
    Entity<Workspace>,
    Entity<DataDocument>,
    &mut VisualTestContext,
) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    cx.update(datatab_ui_base::keymap::init_keymap);

    let (app_state, profile_id) = register_stub_document_connection(cx);

    let workspace_holder: std::rc::Rc<std::cell::RefCell<Option<Entity<Workspace>>>> =
        Default::default();
    let document_holder: std::rc::Rc<std::cell::RefCell<Option<Entity<DataDocument>>>> =
        Default::default();
    let workspace_ref = workspace_holder.clone();
    let document_ref = document_holder.clone();

    let (_, window) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(app_state.clone(), window, cx));
        workspace_ref.replace(Some(workspace.clone()));

        let document = cx.new(|cx| {
            DataDocument::new_for_collection(
                profile_id,
                datatab_core::CollectionRef::new("shop", "orders"),
                app_state.clone(),
                window,
                cx,
            )
        });
        document_ref.replace(Some(document.clone()));

        workspace.update(cx, |workspace, cx| {
            let pane = DataDocument::into_pane(document, cx);
            workspace.tab_manager.update(cx, |manager, cx| {
                manager.open(Tab::Pane(Box::new(pane)), cx);
            });
        });

        gpui_component::Root::new(workspace, window, cx)
    });

    let workspace = workspace_holder
        .borrow()
        .clone()
        .expect("workspace should be created");
    let document = document_holder
        .borrow()
        .clone()
        .expect("document should be created");

    window.run_until_parked();
    (workspace, document, window)
}

/// The first click into the document area after the sidebar held the
/// keyboard must leave the keyboard on the query-bar field the click landed
/// on. The workspace used to refocus the active document on that click,
/// which took the keyboard away from the very editor it had just focused:
/// every typed letter then ran a grid shortcut and no filter could be
/// entered on a document collection (#1234).
#[gpui::test]
fn first_click_into_the_document_area_keeps_the_query_bar_field(cx: &mut TestAppContext) {
    let (workspace, document, window) = new_document_workspace(cx);

    // The sidebar owns the keyboard, as after the user picked the collection.
    window.update(|window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.set_focus(FocusTarget::Sidebar, window, cx)
        })
    });
    window.run_until_parked();
    window.update(|window, _| window.activate_window());

    let slot = window
        .debug_bounds("collection-slot-filter")
        .expect("the document query bar renders its filter slot");
    // The editor fills the slot behind the keyword; click inside it.
    let inside_editor = gpui::point(slot.right() - px(20.0), slot.center().y);
    window.simulate_click(inside_editor, gpui::Modifiers::none());
    window.run_until_parked();

    let context = window.update(|_, cx| {
        document
            .read(cx)
            .data_grid_for_tests()
            .read(cx)
            .active_context(cx)
    });
    assert_eq!(
        context,
        ContextId::TextInput,
        "the first click into the document area must leave the filter field holding the keyboard"
    );
}

use gpui::px;

#[gpui::test]
fn document_focus_request_from_sidebar_focuses_the_document(cx: &mut TestAppContext) {
    let (workspace, app_state, window) = super::new_workspace(cx);
    super::open_code_tab(window, &workspace, &app_state);
    window.update(|window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.set_focus(FocusTarget::Sidebar, window, cx);
        });
    });
    window.run_until_parked();
    let sidebar_focus = window.update(|window, cx| window.focused(cx));
    window.update(|_, cx| {
        let manager = workspace.read(cx).tab_manager.clone();
        manager.update(cx, |_, cx| {
            cx.emit(crate::ui::document::TabManagerEvent::DocumentRequestedFocus);
        });
    });
    window.run_until_parked();
    window.update(|window, cx| {
        assert_ne!(window.focused(cx), sidebar_focus);
        assert_eq!(workspace.read(cx).focus_target, FocusTarget::Document);
    });
}

#[gpui::test]
fn grid_click_then_shift_right_extends_selection(cx: &mut TestAppContext) {
    use datatab_core::{ColumnKind, ColumnMeta, QueryResult, Value};
    let (workspace, app_state, window) = super::new_workspace(cx);
    let columns = ["first", "second", "third"]
        .into_iter()
        .map(|name| ColumnMeta {
            name: name.to_string(),
            type_name: "TEXT".to_string(),
            kind: ColumnKind::Text,
            nullable: false,
            is_primary_key: false,
        })
        .collect();
    let result = QueryResult::table(
        columns,
        vec![vec![
            Value::Text("one".into()),
            Value::Text("two".into()),
            Value::Text("three".into()),
        ]],
        None,
        std::time::Duration::ZERO,
    );
    window.update(|window, cx| {
        let document =
            cx.new(|cx| DataDocument::new_for_test_table(result, vec![], app_state, window, cx));
        workspace.update(cx, |workspace, cx| {
            let pane = DataDocument::into_pane(document, cx);
            workspace.tab_manager.update(cx, |manager, cx| {
                manager.open(Tab::Pane(Box::new(pane)), cx)
            });
            workspace.set_focus(FocusTarget::Sidebar, window, cx);
        });
        window.activate_window();
    });
    window.run_until_parked();
    let header = window
        .debug_bounds("table-header")
        .expect("table header renders");
    let cell = gpui::point(header.left() + px(60.0), header.bottom() + px(15.0));
    window.simulate_click(cell, gpui::Modifiers::none());
    window.run_until_parked();
    window.simulate_keystrokes("ctrl-c");
    let initial = window
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text());
    assert_eq!(
        initial.as_deref(),
        Some("one"),
        "the click selects the first cell"
    );
    window.simulate_keystrokes("shift-right shift-right ctrl-c");
    let copied = window
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("one\ttwo\tthree"));
}

fn register_reserved_collection_connection(
    cx: &mut TestAppContext,
) -> (Entity<AppStateEntity>, uuid::Uuid) {
    let (app_state, profile_id) = register_stub_document_connection(cx);
    cx.update(|cx| {
        app_state.update(cx, |state, _| {
            let connected = state
                .connections_mut()
                .get_mut(&profile_id)
                .expect("connected fixture");
            connected.schema = Some(datatab_core::SchemaSnapshot::document(
                datatab_core::DocumentSchema {
                    databases: Vec::new(),
                    current_database: Some("shop".to_string()),
                    collections: vec![datatab_core::CollectionInfo {
                        name: "events".to_string(),
                        database: Some("shop".to_string()),
                        document_count: None,
                        avg_document_size: None,
                        sample_fields: None,
                        indexes: None,
                        validator: None,
                        is_capped: false,
                        presentation: datatab_core::CollectionPresentation::ReservedEventStream,
                        child_items: None,
                    }],
                },
            ));
        });
    });
    (app_state, profile_id)
}

#[gpui::test]
fn reserved_event_stream_collection_opens_as_data_grid(cx: &mut TestAppContext) {
    use crate::ui::views::workspace::actions::TabPlacement;
    use std::cell::RefCell;
    use std::rc::Rc;

    let (app_state, profile_id) = register_reserved_collection_connection(cx);
    let (workspace, _, window) = super::new_workspace_with(cx, app_state);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let subscription = window.update(|_, cx| {
        let opened = opened.clone();
        cx.observe_new::<DataDocument>(move |_, _, cx| {
            opened.borrow_mut().push(cx.entity());
        })
    });
    let collection = datatab_core::CollectionRef::new("shop", "events");
    for _ in 0..2 {
        window.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_collection_document(
                    profile_id,
                    collection.clone(),
                    TabPlacement::ReuseExisting,
                    window,
                    cx,
                );
            });
        });
        window.run_until_parked();
    }
    assert_eq!(
        opened.borrow().len(),
        1,
        "reserved presentation opens a grid and reuses it"
    );
    window.update(|window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.open_collection_document(
                profile_id,
                collection.clone(),
                TabPlacement::NewTab,
                window,
                cx,
            );
        });
    });
    window.run_until_parked();
    assert_eq!(
        opened.borrow().len(),
        2,
        "New Tab preserves the explicit duplicate request"
    );
    drop(subscription);
}
