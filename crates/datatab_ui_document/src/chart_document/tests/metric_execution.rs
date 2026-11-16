use super::super::*;
use async_trait::async_trait;
use datatab_core::{
    ColumnKind, ColumnMeta, Connection, DbError, DriverMetadata, InstanceCatalog,
    InstanceInspectorDef, InstanceMetricDef, InstanceMetricKind, InstanceMetricUnit, QueryHandle,
    QueryRequest, SchemaSnapshot, Value,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

type CatalogCalls = Arc<Mutex<Vec<std::thread::ThreadId>>>;

struct TestHarness;

impl Render for TestHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
    }
}

struct Catalog {
    listed: bool,
}

#[async_trait]
impl InstanceCatalog for Catalog {
    fn metric_kind(&self, metric_id: &str) -> Option<InstanceMetricKind> {
        (metric_id == "test.counter").then_some(InstanceMetricKind::Counter)
    }
    async fn list_metrics(&self) -> Result<Vec<InstanceMetricDef>, DbError> {
        Ok(if self.listed {
            vec![InstanceMetricDef {
                id: "test.counter".into(),
                display_name: "counter".into(),
                group: "test".into(),
                unit: InstanceMetricUnit::PerSecond,
                kind: InstanceMetricKind::Counter,
                description: None,
                default_refresh_secs: 15,
            }]
        } else {
            vec![]
        })
    }
    async fn list_inspectors(&self) -> Result<Vec<InstanceInspectorDef>, DbError> {
        Ok(vec![])
    }
    async fn fetch_metric_series(&self, _: &str, _: i64, _: i64) -> Result<QueryResult, DbError> {
        unreachable!()
    }
    async fn fetch_inspector_snapshot(&self, _: &str) -> Result<QueryResult, DbError> {
        unreachable!()
    }
}

struct FakeConnection {
    listed: bool,
    calls: AtomicUsize,
    catalog_calls: CatalogCalls,
}
impl Connection for FakeConnection {
    fn metadata(&self) -> &DriverMetadata {
        static META: OnceLock<DriverMetadata> = OnceLock::new();
        META.get_or_init(|| DriverMetadata {
            id: "stub".into(),
            display_name: "Stub".into(),
            description: "test".into(),
            category: datatab_core::DatabaseCategory::Relational,
            transfer_family: datatab_core::TransferFamily::Sql,
            deployment_class: None,
            query_language: datatab_core::QueryLanguage::Sql,
            capabilities: datatab_core::DriverCapabilities::empty(),
            default_port: None,
            uri_scheme: "stub".into(),
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
        })
    }
    fn ping(&self) -> Result<(), DbError> {
        Ok(())
    }
    fn close(&mut self) -> Result<(), DbError> {
        Ok(())
    }
    fn execute(&self, _: &QueryRequest) -> Result<QueryResult, DbError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) as i64;
        let mut result = QueryResult::empty();
        result.columns = vec![
            ColumnMeta {
                name: "time".into(),
                type_name: "int8".into(),
                kind: ColumnKind::Timestamp,
                nullable: false,
                is_primary_key: false,
            },
            ColumnMeta {
                name: "value".into(),
                type_name: "float8".into(),
                kind: ColumnKind::Float,
                nullable: false,
                is_primary_key: false,
            },
        ];
        result.rows = vec![vec![
            Value::Int(1000 + 2000 * n),
            Value::Float(100.0 + 20.0 * n as f64),
        ]];
        Ok(result)
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
    fn schema_loading_strategy(&self) -> datatab_core::SchemaLoadingStrategy {
        datatab_core::SchemaLoadingStrategy::SingleDatabase
    }
    #[expect(
        clippy::unimplemented,
        reason = "the metric fetch path does not use a dialect"
    )]
    fn dialect(&self) -> &dyn datatab_core::SqlDialect {
        unimplemented!()
    }
    fn instance_catalog(&self) -> Option<Box<dyn InstanceCatalog>> {
        self.catalog_calls
            .lock()
            .expect("catalog calls")
            .push(std::thread::current().id());
        Some(Box::new(Catalog {
            listed: self.listed,
        }))
    }
}

fn setup(
    cx: &mut gpui::TestAppContext,
    listed: bool,
) -> (
    Entity<ChartDocument>,
    &mut gpui::VisualTestContext,
    CatalogCalls,
) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    cx.update(|cx| {
        let host = cx.new(|_| datatab_ui_base::toast::ToastHost::new());
        cx.set_global(datatab_ui_base::toast::ToastGlobal { host });
    });
    let id = Uuid::new_v4();
    let catalog_calls = Arc::new(Mutex::new(Vec::new()));
    let connection_catalog_calls = catalog_calls.clone();
    let state = cx.update(|cx| {
        cx.new(|_| {
            let storage =
                datatab_storage::bootstrap::StorageRuntime::in_memory().expect("in-memory storage");
            AppStateEntity::new_with_storage_runtime(storage).expect("app state")
        })
    });
    cx.update(|cx| {
        state.update(cx, |app, _| {
            let profile = datatab_core::ConnectionProfile::new(
                "test",
                datatab_core::DbConfig::SQLite {
                    path: ":memory:".into(),
                    connection_id: None,
                },
            );
            app.connections_mut().insert(
                id,
                datatab_core::ConnectedProfile {
                    profile,
                    connection: Arc::new(FakeConnection {
                        listed,
                        calls: AtomicUsize::new(0),
                        catalog_calls: connection_catalog_calls,
                    }),
                    schema: None,
                    mutation_policy: Default::default(),
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
                },
            );
        })
    });
    let holder = std::rc::Rc::new(std::cell::RefCell::new(None));
    let target = holder.clone();
    let (_, visual) = cx.add_window_view(move |window, cx| {
        let doc = cx.new(|cx| {
            ChartDocument::new_with_source(
                Some(id),
                "counter".into(),
                Box::new(datatab_components::chart::InstanceMetricSource {
                    metric_id: "test.counter".into(),
                }),
                state,
                window,
                cx,
            )
        });
        doc.update(cx, |chart, _| chart.pending_time_window = Some((0, 10_000)));
        target.replace(Some(doc.clone()));
        let harness = cx.new(|_| TestHarness);
        gpui_component::Root::new(harness, window, cx)
    });
    let doc = holder.borrow().clone().expect("chart");
    (doc, visual, catalog_calls)
}

#[gpui::test]
fn catalog_lookup_is_deferred_from_foreground_request(cx: &mut gpui::TestAppContext) {
    let (doc, visual, catalog_calls) = setup(cx, true);
    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            chart.request_reexecute(window, cx);
            assert!(
                catalog_calls.lock().expect("catalog calls").is_empty(),
                "catalog was called during the foreground request"
            );
        });
    });
    visual.run_until_parked();
    assert_eq!(catalog_calls.lock().expect("catalog calls").len(), 1);
}

#[gpui::test]
fn changing_source_clears_cached_metric_kind(cx: &mut gpui::TestAppContext) {
    let (doc, visual, catalog_calls) = setup(cx, true);
    fetch(&doc, visual, false);
    fetch(&doc, visual, false);
    assert_eq!(catalog_calls.lock().expect("catalog calls").len(), 1);

    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            chart.set_data_source(
                Box::new(datatab_components::chart::InstanceMetricSource {
                    metric_id: "test.counter".into(),
                }),
                window,
                cx,
            );
            assert_eq!(chart.instance_metric_kind, None);
        });
    });
    fetch(&doc, visual, false);
    assert_eq!(catalog_calls.lock().expect("catalog calls").len(), 2);
}

fn fetch(
    doc: &Entity<ChartDocument>,
    visual: &mut gpui::VisualTestContext,
    manual_during_fetch: bool,
) {
    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            chart.request_reexecute(window, cx);
            if manual_during_fetch {
                chart.set_refresh_policy(RefreshPolicy::Manual, cx);
            }
        })
    });
    visual.run_until_parked();
    visual.update(|_, cx| {
        doc.update(cx, |chart, cx| {
            let result = chart.pending_result.take().expect("fetch completed");
            chart.apply_result(result, cx);
        })
    });
}

#[gpui::test]
fn counter_metric_rates_survive_refresh_policy_change(cx: &mut gpui::TestAppContext) {
    let (doc, visual, _) = setup(cx, true);
    visual.update(|_, cx| {
        doc.update(cx, |chart, cx| {
            chart.set_refresh_policy(RefreshPolicy::Interval { every_secs: 10 }, cx)
        })
    });
    fetch(&doc, visual, false);
    fetch(&doc, visual, true);
    fetch(&doc, visual, false);
    visual.update(|_, cx| {
        let result = doc
            .read(cx)
            .instance_metric_buffer
            .as_ref()
            .expect("buffer")
            .to_query_result();
        assert_eq!(
            result.rows,
            vec![vec![Value::Int(5000), Value::Float(10.0)]]
        );
    });
}

#[gpui::test]
fn counter_missing_from_listing_still_fetches_rates(cx: &mut gpui::TestAppContext) {
    let (doc, visual, _) = setup(cx, false);
    fetch(&doc, visual, false);
    fetch(&doc, visual, false);
    visual.update(|_, cx| {
        let result = doc
            .read(cx)
            .instance_metric_buffer
            .as_ref()
            .expect("metric fetch must succeed")
            .to_query_result();
        assert_eq!(
            result.rows,
            vec![vec![Value::Int(3000), Value::Float(10.0)]]
        );
    });
}

#[gpui::test]
fn counter_metric_fetches_and_plots_rates(cx: &mut gpui::TestAppContext) {
    let (doc, visual, _) = setup(cx, true);
    fetch(&doc, visual, false);
    fetch(&doc, visual, false);
    visual.update(|_, cx| {
        let result = doc
            .read(cx)
            .instance_metric_buffer
            .as_ref()
            .expect("buffer")
            .to_query_result();
        assert_eq!(
            result.rows,
            vec![vec![Value::Int(3000), Value::Float(10.0)]]
        );
    });
}

#[gpui::test]
fn source_swap_clears_pending_result(cx: &mut gpui::TestAppContext) {
    let (doc, visual, _) = setup(cx, true);
    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            chart.request_reexecute(window, cx);
        });
    });
    visual.run_until_parked();

    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            assert!(
                chart.pending_result.is_some(),
                "expected pending result before source swap"
            );
            chart.set_data_source(
                Box::new(datatab_components::chart::InstanceMetricSource {
                    metric_id: "test.gauge".into(),
                }),
                window,
                cx,
            );
            assert!(
                chart.pending_result.is_none(),
                "set_data_source must clear any pending result from the old source"
            );
        });
    });
}

#[gpui::test]
fn apply_result_drops_stale_result_after_source_swap(cx: &mut gpui::TestAppContext) {
    let (doc, visual, _) = setup(cx, true);
    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            chart.request_reexecute(window, cx);
        });
    });
    visual.run_until_parked();

    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            let stale = chart
                .pending_result
                .take()
                .expect("expected pending result from old source");
            chart.set_data_source(
                Box::new(datatab_components::chart::InstanceMetricSource {
                    metric_id: "test.gauge".into(),
                }),
                window,
                cx,
            );
            chart.apply_result(stale, cx);
            assert!(
                chart.last_result.is_none(),
                "stale result from old source must not populate last_result"
            );
            assert!(
                chart.instance_metric_buffer.is_none(),
                "stale result from old source must not populate instance_metric_buffer"
            );
            assert_eq!(
                chart.instance_metric_kind, None,
                "stale result from old source must not populate instance_metric_kind"
            );
        });
    });
}

#[gpui::test]
fn cancelled_current_task_resets_exec_state_and_document_state(cx: &mut gpui::TestAppContext) {
    let (doc, visual, _) = setup(cx, true);
    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            chart.request_reexecute(window, cx);
            assert!(chart.exec_state == ExecState::Running);
            assert_eq!(chart.state, DocumentState::Executing);
        });
    });
    visual.run_until_parked();

    visual.update(|_, cx| {
        doc.update(cx, |chart, cx| {
            let pending = chart
                .pending_result
                .take()
                .expect("expected pending result before task cancellation");
            chart.app_state.update(cx, |state, _| {
                assert!(
                    state.tasks_mut().cancel(pending.task_id),
                    "active task should cancel in task manager"
                );
            });

            chart.apply_result(pending, cx);

            assert!(
                chart.exec_state == ExecState::Idle,
                "cancelling the current task must reset exec_state to Idle"
            );
            assert_eq!(
                chart.state,
                DocumentState::Clean,
                "cancelling the current task must reset document state to Clean"
            );
            assert!(
                !chart.runner.is_primary_active(),
                "cancelled current task must clear the primary task slot"
            );
            assert!(
                chart.last_result.is_none(),
                "cancelled current task must not apply its result"
            );
            assert!(
                chart.instance_metric_buffer.is_none(),
                "cancelled current task must not populate instance_metric_buffer"
            );
        });
    });
}

#[gpui::test]
fn stale_older_task_result_does_not_reset_newer_running_task(cx: &mut gpui::TestAppContext) {
    let (doc, visual, _) = setup(cx, true);
    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            chart.request_reexecute(window, cx);
        });
    });
    visual.run_until_parked();

    visual.update(|window, cx| {
        doc.update(cx, |chart, cx| {
            let stale = chart
                .pending_result
                .take()
                .expect("expected pending result for first task");
            chart.request_reexecute(window, cx);
            assert!(chart.exec_state == ExecState::Running);
            assert_eq!(chart.state, DocumentState::Executing);

            chart.apply_result(stale, cx);

            assert!(
                chart.exec_state == ExecState::Running,
                "older task result must not reset exec_state of newer running task"
            );
            assert_eq!(
                chart.state,
                DocumentState::Executing,
                "older task result must not reset document state of newer running task"
            );
            assert!(
                chart.runner.is_primary_active(),
                "older task result must not clear newer task from primary slot"
            );
            assert!(
                chart.last_result.is_none(),
                "older task result must be dropped"
            );
        });
    });
}
