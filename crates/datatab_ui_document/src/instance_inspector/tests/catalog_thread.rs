//! Catalog lookups of the instance inspector run on the background executor,
//! and a reconnect in between drops their result.

use crate::data_grid_panel::DataGridPanel;
use crate::instance_inspector::{InspectorPanel, PendingKillConfirm};
use crate::types::DocumentState;
use datatab_audit::query::AuditQueryFilter;
use datatab_core::{
    ColumnKind, ColumnMeta, Connection, ConnectionProfile, DatabaseCategory, DbConfig, DbError,
    DriverMetadata, DriverMetadataBuilder, InspectorRowAction, InstanceCatalog,
    InstanceInspectorDef, InstanceMetricDef, QueryHandle, QueryLanguage, QueryRequest, QueryResult,
    SchemaLoadingStrategy, SchemaSnapshot, SqlDialect, Value, WritePrivilege,
};
use datatab_ui_base::AppStateEntity;
use gpui::{AppContext as _, Entity, Render as _, TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use uuid::Uuid;

const METRIC_ID: &str = "sessions";

struct FakeCatalog {
    actions: Vec<InspectorRowAction>,
    executed_actions: Arc<AtomicUsize>,
    fail_actions: bool,
}

#[async_trait::async_trait]
impl InstanceCatalog for FakeCatalog {
    async fn list_metrics(&self) -> Result<Vec<InstanceMetricDef>, DbError> {
        Ok(Vec::new())
    }

    async fn list_inspectors(&self) -> Result<Vec<InstanceInspectorDef>, DbError> {
        Ok(Vec::new())
    }

    async fn fetch_metric_series(&self, _: &str, _: i64, _: i64) -> Result<QueryResult, DbError> {
        Err(DbError::NotSupported("unused".into()))
    }

    async fn fetch_inspector_snapshot(&self, _: &str) -> Result<QueryResult, DbError> {
        Err(DbError::NotSupported("unused".into()))
    }

    fn row_actions(&self, _metric_id: &str) -> Vec<InspectorRowAction> {
        self.actions.clone()
    }

    async fn execute_row_action(
        &self,
        _metric_id: &str,
        _action_id: &str,
        _row_values: &[Value],
    ) -> Result<(), DbError> {
        self.executed_actions.fetch_add(1, Ordering::SeqCst);
        if self.fail_actions {
            return Err(DbError::Timeout);
        }
        Ok(())
    }
}

/// Counts how often the inspector probes the connection, and how often a row
/// action ran on it.
struct FakeConnection {
    metadata: DriverMetadata,
    catalog_calls: Arc<AtomicUsize>,
    executed_actions: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
    fail_actions: bool,
}

impl Connection for FakeConnection {
    fn metadata(&self) -> &DriverMetadata {
        &self.metadata
    }

    fn ping(&self) -> Result<(), DbError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), DbError> {
        Ok(())
    }

    fn execute(&self, _: &QueryRequest) -> Result<QueryResult, DbError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(QueryResult::empty())
    }

    fn cancel(&self, _: &QueryHandle) -> Result<(), DbError> {
        Ok(())
    }

    fn schema(&self) -> Result<SchemaSnapshot, DbError> {
        Ok(SchemaSnapshot::default())
    }

    fn kind(&self) -> datatab_core::DbKind {
        datatab_core::DbKind::SQLite
    }

    fn schema_loading_strategy(&self) -> SchemaLoadingStrategy {
        SchemaLoadingStrategy::SingleDatabase
    }

    fn dialect(&self) -> &dyn SqlDialect {
        &datatab_core::DefaultSqlDialect
    }

    fn instance_catalog(&self) -> Option<Box<dyn InstanceCatalog>> {
        self.catalog_calls.fetch_add(1, Ordering::SeqCst);
        Some(Box::new(FakeCatalog {
            actions: vec![InspectorRowAction {
                id: "kill".to_string(),
                label: "Kill session".to_string(),
                description: None,
                is_destructive: true,
            }],
            executed_actions: self.executed_actions.clone(),
            fail_actions: self.fail_actions,
        }))
    }
}

struct FakeProfile {
    connection: Arc<FakeConnection>,
    catalog_calls: Arc<AtomicUsize>,
    executed_actions: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
}

fn fake_profile() -> FakeProfile {
    fake_profile_with_actions(false)
}

/// With `fail_actions` every row action fails, so a kill completion carries an
/// error instead of a success.
fn fake_profile_with_actions(fail_actions: bool) -> FakeProfile {
    let catalog_calls = Arc::new(AtomicUsize::new(0));
    let executed_actions = Arc::new(AtomicUsize::new(0));
    let queries = Arc::new(AtomicUsize::new(0));
    let connection = Arc::new(FakeConnection {
        metadata: DriverMetadataBuilder::new(
            "fake",
            "Fake",
            DatabaseCategory::Relational,
            QueryLanguage::Sql,
        )
        .build(),
        catalog_calls: catalog_calls.clone(),
        executed_actions: executed_actions.clone(),
        queries: queries.clone(),
        fail_actions,
    });
    FakeProfile {
        connection,
        catalog_calls,
        executed_actions,
        queries,
    }
}

fn connect(
    profile: &ConnectionProfile,
    connection: &FakeProfile,
    state: &Entity<AppStateEntity>,
    cx: &mut gpui::App,
) {
    let profile = profile.clone();
    let connection = connection.connection.clone();
    state.update(cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None);
        state.apply_connect_profile(
            profile,
            connection,
            None,
            None,
            false,
            WritePrivilege::Unknown,
        );
    });
}

fn reconnect(state: &Entity<AppStateEntity>, profile_id: Uuid, cx: &mut gpui::App) -> FakeProfile {
    let replacement = fake_profile();
    let connection = replacement.connection.clone();
    state.update(cx, |state, _| {
        let profile = state
            .connections()
            .get(&profile_id)
            .map(|connected| connected.profile.clone())
            .expect("profile should be connected before reconnect");
        if let Some(teardown) = state.disconnect(profile_id) {
            teardown
                .join()
                .expect("teardown thread should finish")
                .expect("fake connection close should succeed");
        }
        state.apply_connect_profile(
            profile,
            connection,
            None,
            None,
            false,
            WritePrivilege::Unknown,
        );
    });
    replacement
}

struct Fixture {
    panel: Entity<InspectorPanel>,
    app_state: Entity<AppStateEntity>,
    profile_id: Uuid,
    catalog_calls: Arc<AtomicUsize>,
    executed_actions: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
    window: VisualTestContext,
}

/// Connects a fake profile and builds an inspector window. With `with_table`
/// the first render creates the table and starts the row-action lookup.
fn setup(cx: &mut TestAppContext, with_table: bool) -> Fixture {
    setup_with(cx, with_table, false)
}

/// Like `setup`, with every row action failing.
fn setup_with_failing_kill(cx: &mut TestAppContext) -> Fixture {
    setup_with(cx, false, true)
}

fn setup_with(cx: &mut TestAppContext, with_table: bool, fail_actions: bool) -> Fixture {
    cx.update(datatab_components::theme::init);
    cx.update(|cx| {
        let host = cx.new(|_| datatab_ui_base::toast::ToastHost::new());
        cx.set_global(datatab_ui_base::toast::ToastGlobal { host });
    });

    let app_state: Entity<AppStateEntity> = cx.update(|cx| {
        cx.new(|_| {
            let runtime =
                datatab_storage::bootstrap::StorageRuntime::in_memory().expect("in-memory storage");
            AppStateEntity::new_with_storage_runtime(runtime).expect("app state")
        })
    });

    let profile = ConnectionProfile::new(
        "fake-inspector",
        DbConfig::SQLite {
            path: ":memory:".into(),
            connection_id: None,
        },
    );
    let profile_id = profile.id;
    let fake = fake_profile_with_actions(fail_actions);
    cx.update(|cx| connect(&profile, &fake, &app_state, cx));

    let result = QueryResult::table(
        vec![ColumnMeta {
            name: "pid".to_string(),
            type_name: "int4".to_string(),
            kind: ColumnKind::Integer,
            nullable: false,
            is_primary_key: false,
        }],
        vec![vec![Value::Int(101)]],
        None,
        Duration::ZERO,
    );

    let holder: Rc<RefCell<Option<Entity<InspectorPanel>>>> = Rc::default();
    let target = holder.clone();
    let window_state = app_state.clone();
    let window_profile = profile_id;
    let handle = cx.add_window(move |window, cx| {
        let panel = cx
            .new(|cx| InspectorPanel::new(window_profile, METRIC_ID.to_string(), window_state, cx));
        if with_table {
            panel.update(cx, |panel, cx| {
                panel.show_result_for_test(result, cx);
                // Build the table now, so the test sees the lookup this render
                // starts without waiting for the window's first draw.
                panel.render(window, cx);
            });
        }
        target.replace(Some(panel.clone()));
        gpui_component::Root::new(panel, window, cx)
    });
    let panel = holder.borrow().clone().expect("the window builds a panel");
    let window = VisualTestContext::from_window(handle.into(), cx);

    Fixture {
        panel,
        app_state,
        profile_id,
        catalog_calls: fake.catalog_calls,
        executed_actions: fake.executed_actions,
        queries: fake.queries,
        window,
    }
}

fn grid(fixture: &mut Fixture) -> Entity<DataGridPanel> {
    fixture
        .window
        .update(|_, cx| fixture.panel.read(cx).data_grid.clone())
        .expect("the rendered result builds a table")
}

fn menu_row_actions(
    fixture: &mut Fixture,
    grid: &Entity<DataGridPanel>,
) -> Vec<InspectorRowAction> {
    fixture
        .window
        .update(|_, cx| grid.read(cx).menu_row_actions())
}

fn confirm_kill(fixture: &mut Fixture) {
    fixture.window.update(|_, cx| {
        fixture.panel.update(cx, |panel, cx| {
            panel.pending_kill_confirm = Some(PendingKillConfirm {
                action_id: "kill".to_string(),
                action_label: "Kill session".to_string(),
                row_values: vec![Value::Int(101)],
            });
            panel.confirm_kill_action(cx);
        });
    });
}

/// The audit actions recorded for inspector row actions, in insertion order.
fn kill_audit_actions(fixture: &mut Fixture) -> Vec<String> {
    let app_state = fixture.app_state.clone();
    fixture.window.update(|_, cx| {
        app_state
            .read(cx)
            .audit_service()
            .query_extended(&AuditQueryFilter::default())
            .expect("audit query")
            .into_iter()
            .filter_map(|event| event.action)
            .filter(|action| action.starts_with("inspector_row_action"))
            .collect()
    })
}

fn toast_count(fixture: &mut Fixture) -> usize {
    fixture.window.update(|_, cx| {
        cx.global::<datatab_ui_base::toast::ToastGlobal>()
            .host
            .read(cx)
            .toast_count()
    })
}

#[gpui::test]
fn row_action_lookup_is_deferred_from_the_foreground(cx: &mut TestAppContext) {
    let mut fixture = setup(cx, true);
    let grid = grid(&mut fixture);

    assert_eq!(
        fixture.catalog_calls.load(Ordering::SeqCst),
        0,
        "instance_catalog must not run while the foreground builds the table"
    );
    assert!(
        menu_row_actions(&mut fixture, &grid).is_empty(),
        "the row actions are not known before the background lookup returns"
    );

    fixture.window.run_until_parked();

    assert_eq!(fixture.catalog_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        menu_row_actions(&mut fixture, &grid)
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        ["kill"]
    );
}

#[gpui::test]
fn row_action_lookup_drops_its_result_after_a_reconnect(cx: &mut TestAppContext) {
    let mut fixture = setup(cx, true);
    let grid = grid(&mut fixture);

    let replacement = fixture
        .window
        .update(|_, cx| reconnect(&fixture.app_state, fixture.profile_id, cx));
    fixture.window.run_until_parked();

    assert_eq!(
        fixture.catalog_calls.load(Ordering::SeqCst),
        1,
        "the lookup still runs on the connection it captured"
    );
    assert_eq!(
        replacement.catalog_calls.load(Ordering::SeqCst),
        0,
        "the new connection is never probed"
    );
    assert!(
        menu_row_actions(&mut fixture, &grid).is_empty(),
        "row actions from an earlier connection must not be installed after a reconnect"
    );
}

#[gpui::test]
fn kill_action_lookup_deferral_regression_guard(cx: &mut TestAppContext) {
    let mut fixture = setup(cx, false);

    confirm_kill(&mut fixture);

    assert_eq!(
        fixture.catalog_calls.load(Ordering::SeqCst),
        0,
        "instance_catalog must not run inside the foreground confirmation"
    );
    assert_eq!(fixture.executed_actions.load(Ordering::SeqCst), 0);

    fixture.window.run_until_parked();

    assert!(
        fixture.catalog_calls.load(Ordering::SeqCst) >= 1,
        "the background lookup runs after the foreground confirmation returns"
    );
    assert_eq!(
        fixture.executed_actions.load(Ordering::SeqCst),
        1,
        "the confirmed action runs once the background lookup returns"
    );
    assert_eq!(
        fixture.queries.load(Ordering::SeqCst),
        1,
        "a completed action refreshes the panel on its own connection"
    );
}

#[gpui::test]
fn kill_action_drops_its_result_after_a_reconnect(cx: &mut TestAppContext) {
    let mut fixture = setup(cx, false);

    confirm_kill(&mut fixture);

    let replacement = fixture
        .window
        .update(|_, cx| reconnect(&fixture.app_state, fixture.profile_id, cx));
    fixture.window.run_until_parked();

    assert_eq!(
        replacement.queries.load(Ordering::SeqCst),
        0,
        "a stale action result must not refresh the panel through the new connection"
    );
    assert_eq!(
        fixture.executed_actions.load(Ordering::SeqCst),
        1,
        "the action still runs on the connection it captured"
    );
    assert_eq!(
        fixture
            .window
            .update(|_, cx| fixture.panel.read(cx).state()),
        DocumentState::Clean,
        "a stale action result must not change what the panel shows"
    );
    assert_eq!(
        kill_audit_actions(&mut fixture),
        ["inspector_row_action"],
        "a kill that ran still writes its audit row when the profile reconnects first"
    );
}

#[gpui::test]
fn failed_kill_action_is_reported_after_a_reconnect(cx: &mut TestAppContext) {
    let mut fixture = setup_with_failing_kill(cx);

    confirm_kill(&mut fixture);

    let replacement = fixture
        .window
        .update(|_, cx| reconnect(&fixture.app_state, fixture.profile_id, cx));
    fixture.window.run_until_parked();

    assert_eq!(
        replacement.queries.load(Ordering::SeqCst),
        0,
        "a stale action result must not refresh the panel through the new connection"
    );
    assert_eq!(
        fixture.executed_actions.load(Ordering::SeqCst),
        1,
        "the failing action still runs on the connection it captured"
    );
    assert_eq!(
        toast_count(&mut fixture),
        1,
        "a kill that failed still tells the user when the profile reconnects first"
    );
    assert_eq!(
        kill_audit_actions(&mut fixture),
        ["inspector_row_action_failed"],
        "a failed kill still writes its audit row when the profile reconnects first"
    );
    assert_eq!(
        fixture
            .window
            .update(|_, cx| fixture.panel.read(cx).state()),
        DocumentState::Clean,
        "a stale failure must not change what the panel shows"
    );
}
