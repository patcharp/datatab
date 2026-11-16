use super::*;
use crate::ui::labels::{
    charts_instance_overview_create_editable_failed_message,
    charts_instance_overview_created_editable_message, charts_instance_overview_editable_name,
    charts_instance_overview_no_dashboard_message,
};
use datatab_core::LogErr;
use datatab_ui_base::ConnectionSession;

impl Workspace {
    /// Open a `ChartDocument` for an instance metric leaf clicked in the sidebar.
    ///
    /// Deduplicates by `(profile_id, metric_id)` using `DocumentKey::InstanceMetric`
    /// so clicking the same metric a second time focuses the existing tab rather
    /// than opening a duplicate.
    pub(in crate::ui::views::workspace) fn open_instance_metric(
        &mut self,
        profile_id: uuid::Uuid,
        metric_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::document::{ChartDocument, DocumentKey};
        use datatab_components::chart::InstanceMetricSource;

        let key = DocumentKey::InstanceMetric {
            profile_id,
            metric_id: metric_id.clone(),
        };

        if let Some(existing_id) = self.tab_manager.read(cx).find_by_key(&key, cx) {
            self.tab_manager.update(cx, |mgr, cx| {
                mgr.activate(existing_id, cx);
            });
            self.set_focus(FocusTarget::Document, window, cx);
            return;
        }

        let source = InstanceMetricSource {
            metric_id: metric_id.clone(),
        };

        let title = metric_id.clone();
        let metric_id_for_identity = metric_id.clone();
        let doc = cx.new(|cx| {
            let mut chart = ChartDocument::new_with_source(
                Some(profile_id),
                title,
                Box::new(source),
                self.app_state.clone(),
                window,
                cx,
            );
            chart.set_instance_metric_identity(metric_id_for_identity);
            // InstanceMetric sources poll at 10-second intervals and default to a
            // 15-minute rolling window (index 0 in TimeRangePanel's preset list).
            chart.set_initial_time_range_preset(0);
            chart
        });

        doc.update(cx, |chart, cx| {
            chart.set_refresh_policy(datatab_core::RefreshPolicy::Interval { every_secs: 10 }, cx);
        });

        let pane = ChartDocument::into_pane(doc, cx);
        self.tab_manager.update(cx, |mgr, cx| {
            mgr.open(Tab::Pane(Box::new(pane)), cx);
        });
        self.set_focus(FocusTarget::Document, window, cx);
    }

    /// Open an `InspectorPanel` for an instance inspector leaf clicked in the sidebar.
    ///
    /// Deduplicates by `(profile_id, metric_id)` using `DocumentKey::InstanceInspector`
    /// so clicking the same inspector a second time focuses the existing tab rather
    /// than opening a duplicate.
    pub(in crate::ui::views::workspace) fn open_instance_inspector(
        &mut self,
        profile_id: uuid::Uuid,
        metric_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::document::{DocumentKey, InspectorPanel};

        let key = DocumentKey::InstanceInspector {
            profile_id,
            metric_id: metric_id.clone(),
        };

        if let Some(existing_id) = self.tab_manager.read(cx).find_by_key(&key, cx) {
            self.tab_manager.update(cx, |mgr, cx| {
                mgr.activate(existing_id, cx);
            });
            self.set_focus(FocusTarget::Document, window, cx);
            return;
        }

        let doc =
            cx.new(|cx| InspectorPanel::new(profile_id, metric_id, self.app_state.clone(), cx));

        doc.update(cx, |panel, cx| {
            panel.request_reexec(cx);
        });

        let pane = InspectorPanel::into_pane(doc, cx);
        self.tab_manager.update(cx, |mgr, cx| {
            mgr.open(Tab::Pane(Box::new(pane)), cx);
        });
        self.set_focus(FocusTarget::Document, window, cx);
    }

    fn capture_connection_session(
        &self,
        profile_id: uuid::Uuid,
        cx: &App,
    ) -> Option<ConnectionSession> {
        self.app_state
            .read(cx)
            .capture_connection_session(profile_id)
    }

    fn is_same_connection_session(
        &self,
        profile_id: uuid::Uuid,
        expected: &ConnectionSession,
        cx: &App,
    ) -> bool {
        self.app_state
            .read(cx)
            .is_same_connection_session(profile_id, expected)
    }

    /// Open the synthesized read-only "Instance Overview" dashboard for a profile.
    ///
    /// The dashboard layout is produced by the driver's `InstanceCatalog::default_dashboard()`
    /// at open time — no rows are written to the database. The resulting tab is
    /// marked read-only so the user cannot mutate it; "Save as" produces an editable copy.
    pub(in crate::ui::views::workspace) fn open_instance_overview(
        &mut self,
        profile_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::document::DocumentKey;

        // Stable synthetic UUID for dedup — derived so the same profile always
        // opens the same overview tab.
        let dashboard_id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("instance_overview:{profile_id}").as_bytes(),
        );

        let key = DocumentKey::InstanceOverview { profile_id };

        if let Some(existing_id) = self.tab_manager.read(cx).find_by_key(&key, cx) {
            self.tab_manager.update(cx, |mgr, cx| {
                mgr.activate(existing_id, cx);
            });
            self.set_focus(FocusTarget::Document, window, cx);
            return;
        }

        let Some(expected_session) = self.capture_connection_session(profile_id, cx) else {
            datatab_ui_base::toast::Toast::info(charts_instance_overview_no_dashboard_message())
                .meta_right(now_hms())
                .push(cx);
            return;
        };
        let connection = expected_session.connection.clone();

        // `instance_catalog()` probes privileges on the client connection; keep
        // it off the foreground thread.
        let background = cx.background_executor().spawn(async move {
            connection
                .instance_catalog()
                .and_then(|catalog| catalog.default_dashboard())
        });

        cx.spawn_in(window, async move |this, cx| {
            let descriptor = background.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_open_instance_overview(
                    profile_id,
                    dashboard_id,
                    expected_session,
                    descriptor,
                    window,
                    cx,
                );
            })
            .log_err();
        })
        .detach();
    }

    fn finish_open_instance_overview(
        &mut self,
        profile_id: uuid::Uuid,
        dashboard_id: uuid::Uuid,
        expected_session: ConnectionSession,
        descriptor: Option<datatab_core::DefaultInstanceDashboard>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::document::{DashboardDocument, DocumentKey, dashboard::DashboardPanelSlot};
        use datatab_components::common::time_range::{TimeRange, TimeRangePanel};
        use datatab_components::saved_chart::{SavedChartRefreshPolicy, TimeRangePreset};

        if !self.is_same_connection_session(profile_id, &expected_session, cx) {
            return;
        }

        let key = DocumentKey::InstanceOverview { profile_id };
        if let Some(existing_id) = self.tab_manager.read(cx).find_by_key(&key, cx) {
            self.tab_manager.update(cx, |mgr, cx| {
                mgr.activate(existing_id, cx);
            });
            self.set_focus(FocusTarget::Document, window, cx);
            return;
        }

        let Some(descriptor) = descriptor else {
            datatab_ui_base::toast::Toast::info(charts_instance_overview_no_dashboard_message())
                .meta_right(now_hms())
                .push(cx);
            return;
        };

        let panel_slots: Vec<DashboardPanelSlot> = descriptor
            .panels
            .iter()
            .map(|panel_def| self.build_overview_panel_slot(profile_id, panel_def, window, cx))
            .collect();

        let shared_time_range = cx.new(|cx| {
            TimeRangePanel::new(
                TimeRangePanel::preset_label(TimeRange::Last15min),
                Some(0),
                window,
                cx,
            )
        });

        let doc = cx.new(|cx| {
            let mut dashboard = DashboardDocument::new(
                dashboard_id,
                descriptor.title.clone(),
                panel_slots,
                shared_time_range,
                Some(TimeRangePreset::Last15min),
                SavedChartRefreshPolicy::Interval { every_secs: 10 },
                true,
                self.app_state.clone(),
                cx,
            );
            dashboard.set_profile_id(profile_id);
            dashboard
        });

        let pane = DashboardDocument::into_pane(doc, cx);
        self.tab_manager.update(cx, |mgr, cx| {
            mgr.open(Tab::Pane(Box::new(pane)), cx);
        });
        self.set_focus(FocusTarget::Document, window, cx);
    }

    fn build_overview_panel_slot(
        &self,
        profile_id: uuid::Uuid,
        panel_def: &datatab_core::DefaultDashboardPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::ui::document::dashboard::DashboardPanelSlot {
        use crate::ui::document::{ChartDocument, InspectorPanel, dashboard::DashboardPanelSlot};
        use datatab_components::chart::InstanceMetricSource;
        use datatab_ui_document::dashboard::PanelGridPos;

        let grid_pos = PanelGridPos {
            grid_row: panel_def.grid_row,
            grid_column: panel_def.grid_column,
            grid_width: panel_def.grid_width,
            grid_height: panel_def.grid_height,
        };

        if panel_def.is_inspector {
            let metric_id = panel_def.metric_id.clone();
            let app_state = self.app_state.clone();
            let inspector_entity =
                cx.new(|cx| InspectorPanel::new(profile_id, metric_id, app_state, cx));
            inspector_entity.update(cx, |panel, _cx| {
                panel.defer_initial_exec();
            });
            return DashboardPanelSlot::Inspector {
                entity: inspector_entity,
                grid_pos,
                title_override: None,
            };
        }

        let source = InstanceMetricSource {
            metric_id: panel_def.metric_id.clone(),
        };
        let metric_id_clone = panel_def.metric_id.clone();
        let app_state = self.app_state.clone();
        let panel_entity = cx.new(|cx| {
            let mut chart = ChartDocument::new_with_source(
                Some(profile_id),
                metric_id_clone.clone(),
                Box::new(source),
                app_state,
                window,
                cx,
            );
            chart.set_instance_metric_identity(metric_id_clone);
            chart.set_initial_time_range_preset(0);
            chart
        });
        panel_entity.update(cx, |chart, cx| {
            chart.set_embedded(true, cx);
        });
        panel_entity.update(cx, |chart, cx| {
            chart.set_refresh_policy(datatab_core::RefreshPolicy::Interval { every_secs: 10 }, cx);
        });
        DashboardPanelSlot::Loaded {
            panel: panel_entity,
            grid_pos,
            title_override: None,
        }
    }

    /// Clone a read-only instance overview dashboard into a new persisted,
    /// editable dashboard for the same profile, copying all panels.
    ///
    /// For each chart panel in the overview, a `SavedChart` record is upserted
    /// with `source = InstanceMetric { metric_id }`. Inspector panels are
    /// persisted as `DashboardPanelDraft::Inspector`. The new dashboard then
    /// has the same layout as the read-only overview.
    pub(in crate::ui::views::workspace) fn save_overview_as_editable(
        &mut self,
        source_title: String,
        profile_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_name = charts_instance_overview_editable_name(&source_title);
        let Some(expected_session) = self.capture_connection_session(profile_id, cx) else {
            self.finish_save_overview_as_editable(new_name, profile_id, None, None, window, cx);
            return;
        };
        let connection = expected_session.connection.clone();

        // `instance_catalog()` probes privileges on the client connection; keep
        // it off the foreground thread.
        let background = cx.background_executor().spawn(async move {
            connection
                .instance_catalog()
                .and_then(|cat| cat.default_dashboard())
        });

        cx.spawn_in(window, async move |this, cx| {
            let descriptor = background.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_save_overview_as_editable(
                    new_name,
                    profile_id,
                    Some(expected_session),
                    descriptor,
                    window,
                    cx,
                );
            })
            .log_err();
        })
        .detach();
    }

    fn finish_save_overview_as_editable(
        &mut self,
        new_name: String,
        profile_id: uuid::Uuid,
        expected_session: Option<ConnectionSession>,
        descriptor: Option<datatab_core::DefaultInstanceDashboard>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use datatab_components::saved_chart::{SavedChartRefreshPolicy, TimeRangePreset};
        use datatab_ui_base::DashboardPanelDraft;

        if let Some(expected_session) = expected_session.as_ref()
            && !self.is_same_connection_session(profile_id, expected_session, cx)
        {
            return;
        }

        let result = self.app_state.update(cx, |state, _cx| {
            let new_id = state.dashboards.create_dashboard(
                new_name,
                None,
                profile_id,
                Some(TimeRangePreset::Last15min),
                SavedChartRefreshPolicy::Off,
            )?;

            if let Some(descriptor) = descriptor {
                let mut drafts: Vec<DashboardPanelDraft> = Vec::new();
                for panel_def in &descriptor.panels {
                    drafts.push(build_overview_panel_draft(profile_id, panel_def, state)?);
                }

                if !drafts.is_empty() {
                    state.dashboards.append_panels(new_id, drafts)?;
                }
            }

            Ok::<uuid::Uuid, datatab_storage::error::StorageError>(new_id)
        });

        match result {
            Ok(new_id) => {
                Toast::info(charts_instance_overview_created_editable_message())
                    .meta_right(now_hms())
                    .push(cx);
                self.open_dashboard(new_id, window, cx);
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(
                        ErrorKind::Config,
                        charts_instance_overview_create_editable_failed_message(e),
                    ),
                    cx,
                );
            }
        }
    }
}

fn build_overview_panel_draft(
    profile_id: uuid::Uuid,
    panel_def: &datatab_core::DefaultDashboardPanel,
    state: &mut datatab_ui_base::AppStateEntity,
) -> Result<datatab_ui_base::DashboardPanelDraft, datatab_storage::error::StorageError> {
    use datatab_components::chart::{
        AxisKind, AxisSpec, BindingSpec, ChartKind, ChartSpec, YScale,
    };
    use datatab_components::saved_chart::{
        SavedChart, SavedChartRefreshPolicy, SavedChartSource, TimeRangePreset,
    };
    use datatab_core::chrono::Utc;
    use datatab_ui_base::{DashboardPanelDraft, DraftGridLayout};

    let panel_layout = Some(DraftGridLayout {
        grid_row: panel_def.grid_row,
        grid_column: panel_def.grid_column,
        grid_width: panel_def.grid_width,
        grid_height: panel_def.grid_height,
    });

    if panel_def.is_inspector {
        return Ok(DashboardPanelDraft::Inspector {
            metric_id: panel_def.metric_id.clone(),
            layout: panel_layout,
        });
    }

    let now = Utc::now();
    let chart = SavedChart {
        id: uuid::Uuid::new_v4(),
        name: panel_def.metric_id.clone(),
        profile_id,
        source: SavedChartSource::InstanceMetric {
            metric_id: panel_def.metric_id.clone(),
        },
        chart_spec: ChartSpec {
            kind: ChartKind::Line,
            x_axis: AxisSpec {
                column_index: 0,
                label: String::new(),
                kind: AxisKind::Time,
                unit: None,
            },
            series: Vec::new(),
            legend_visible: false,
            decimation_threshold: 500,
            binding: BindingSpec::default(),
            track_source_indices: false,
            y_scale: YScale::Linear,
        },
        bindings: BindingSpec::default(),
        time_range_preset: Some(TimeRangePreset::Last15min),
        refresh_policy: SavedChartRefreshPolicy::Off,
        created_at: now,
        updated_at: now,
    };
    let chart_id = chart.id;
    state.saved_charts.upsert(chart)?;
    Ok(DashboardPanelDraft::Chart {
        saved_chart_id: chart_id,
        layout: panel_layout,
    })
}

#[cfg(test)]
mod tests {
    // Explicit imports, not `use super::*`: the parent glob together with
    // `#[gpui::test]` sends the macro expansion into unbounded recursion.
    use crate::ui::document::DocumentKey;
    use crate::ui::views::workspace::Workspace;
    use datatab_core::{
        Connection, ConnectionProfile, DatabaseCategory, DbConfig, DbError, DbKind,
        DefaultDashboardPanel, DefaultInstanceDashboard, DriverMetadata, DriverMetadataBuilder,
        InstanceCatalog, InstanceInspectorDef, InstanceMetricDef, QueryHandle, QueryLanguage,
        QueryRequest, QueryResult, SchemaLoadingStrategy, SchemaSnapshot, SqlDialect,
        WritePrivilege,
    };
    use datatab_ui_base::AppStateEntity;
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::ThreadId;
    use uuid::Uuid;

    struct FakeCatalog {
        default_dashboard_calls: Arc<AtomicUsize>,
    }

    impl InstanceCatalog for FakeCatalog {
        fn list_metrics<'life0, 'async_trait>(
            &'life0 self,
        ) -> Pin<
            Box<dyn Future<Output = Result<Vec<InstanceMetricDef>, DbError>> + Send + 'async_trait>,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn list_inspectors<'life0, 'async_trait>(
            &'life0 self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<InstanceInspectorDef>, DbError>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn fetch_metric_series<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _metric_id: &'life1 str,
            _start_ms: i64,
            _end_ms: i64,
        ) -> Pin<Box<dyn Future<Output = Result<QueryResult, DbError>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Err(DbError::NotSupported("unused".into())) })
        }

        fn fetch_inspector_snapshot<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _metric_id: &'life1 str,
        ) -> Pin<Box<dyn Future<Output = Result<QueryResult, DbError>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Err(DbError::NotSupported("unused".into())) })
        }

        fn default_dashboard(&self) -> Option<DefaultInstanceDashboard> {
            self.default_dashboard_calls.fetch_add(1, Ordering::SeqCst);
            Some(DefaultInstanceDashboard {
                title: "Instance Overview".to_string(),
                description: None,
                panels: vec![DefaultDashboardPanel {
                    metric_id: "fake.tps".to_string(),
                    is_inspector: false,
                    grid_column: 0,
                    grid_row: 0,
                    grid_width: 6,
                    grid_height: 4,
                }],
            })
        }
    }

    struct FakeConnection {
        metadata: DriverMetadata,
        catalog_calls: Arc<Mutex<Vec<ThreadId>>>,
        default_dashboard_calls: Arc<AtomicUsize>,
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

        fn execute(&self, _request: &QueryRequest) -> Result<QueryResult, DbError> {
            Err(DbError::NotSupported("fake connection".to_string()))
        }

        fn cancel(&self, _handle: &QueryHandle) -> Result<(), DbError> {
            Ok(())
        }

        fn schema(&self) -> Result<SchemaSnapshot, DbError> {
            Ok(SchemaSnapshot::default())
        }

        fn kind(&self) -> DbKind {
            DbKind::SQLite
        }

        fn schema_loading_strategy(&self) -> SchemaLoadingStrategy {
            SchemaLoadingStrategy::SingleDatabase
        }

        fn dialect(&self) -> &dyn SqlDialect {
            &datatab_core::DefaultSqlDialect
        }

        fn instance_catalog(&self) -> Option<Box<dyn InstanceCatalog>> {
            self.catalog_calls
                .lock()
                .expect("catalog calls lock")
                .push(std::thread::current().id());
            Some(Box::new(FakeCatalog {
                default_dashboard_calls: self.default_dashboard_calls.clone(),
            }))
        }
    }

    fn fake_connection() -> (
        Arc<FakeConnection>,
        Arc<Mutex<Vec<ThreadId>>>,
        Arc<AtomicUsize>,
    ) {
        let catalog_calls = Arc::new(Mutex::new(Vec::new()));
        let default_dashboard_calls = Arc::new(AtomicUsize::new(0));
        let connection = Arc::new(FakeConnection {
            metadata: DriverMetadataBuilder::new(
                "fake",
                "Fake",
                DatabaseCategory::Relational,
                QueryLanguage::Sql,
            )
            .build(),
            catalog_calls: catalog_calls.clone(),
            default_dashboard_calls: default_dashboard_calls.clone(),
        });
        (connection, catalog_calls, default_dashboard_calls)
    }

    fn reconnect_profile(
        app_state: &Entity<AppStateEntity>,
        profile_id: Uuid,
        cx: &mut gpui::App,
    ) -> Arc<AtomicUsize> {
        let (new_connection, _, new_default_dashboard_calls) = fake_connection();
        app_state.update(cx, |state, _| {
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
                new_connection,
                None,
                None,
                false,
                WritePrivilege::Unknown,
            );
        });
        new_default_dashboard_calls
    }

    struct TestWorkspace<'a> {
        workspace: Entity<Workspace>,
        app_state: Entity<AppStateEntity>,
        window: &'a mut VisualTestContext,
        profile_id: Uuid,
        catalog_calls: Arc<Mutex<Vec<ThreadId>>>,
        default_dashboard_calls: Arc<AtomicUsize>,
    }

    fn setup_workspace(cx: &mut TestAppContext) -> TestWorkspace<'_> {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        cx.update(datatab_ui_base::keymap::init_keymap);

        let app_state: Entity<AppStateEntity> = cx.update(|cx| {
            cx.new(|_| {
                let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory()
                    .expect("in-memory storage");
                AppStateEntity::new_with_storage_runtime(runtime).expect("test storage setup")
            })
        });

        let holder: Rc<RefCell<Option<Entity<Workspace>>>> = Rc::new(RefCell::new(None));
        let workspace_ref = holder.clone();
        let app_state_for_window = app_state.clone();

        let (_, window) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(app_state_for_window, window, cx));
            workspace_ref.replace(Some(workspace.clone()));
            gpui_component::Root::new(workspace, window, cx)
        });

        let workspace = holder
            .borrow()
            .clone()
            .expect("workspace should be created");

        let (connection, catalog_calls, default_dashboard_calls) = fake_connection();
        let profile = ConnectionProfile::new(
            "fake-instance",
            DbConfig::SQLite {
                path: PathBuf::from(":memory:"),
                connection_id: None,
            },
        );
        let profile_id = profile.id;

        window.update(|_, cx| {
            app_state.update(cx, |state, _| {
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
        });

        TestWorkspace {
            workspace,
            app_state,
            window,
            profile_id,
            catalog_calls,
            default_dashboard_calls,
        }
    }

    #[gpui::test]
    fn open_instance_overview_defers_catalog_lookup_from_foreground(cx: &mut TestAppContext) {
        let TestWorkspace {
            workspace,
            window,
            profile_id,
            catalog_calls,
            default_dashboard_calls,
            ..
        } = setup_workspace(cx);

        window.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.open_instance_overview(profile_id, window, cx);
                assert!(
                    catalog_calls.lock().expect("catalog calls").is_empty(),
                    "instance_catalog must not run synchronously on the foreground thread"
                );
                assert_eq!(default_dashboard_calls.load(Ordering::SeqCst), 0);
            });
        });

        window.run_until_parked();

        assert_eq!(default_dashboard_calls.load(Ordering::SeqCst), 1);
        window.update(|_, cx| {
            let tab_id = workspace
                .read(cx)
                .tab_manager
                .read(cx)
                .find_by_key(&DocumentKey::InstanceOverview { profile_id }, cx);
            assert!(
                tab_id.is_some(),
                "Instance Overview tab should open after background catalog lookup completes"
            );
        });
    }

    #[gpui::test]
    fn save_overview_as_editable_defers_catalog_lookup_from_foreground(cx: &mut TestAppContext) {
        let TestWorkspace {
            workspace,
            app_state,
            window,
            profile_id,
            catalog_calls,
            default_dashboard_calls,
        } = setup_workspace(cx);

        window.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.save_overview_as_editable(
                    "Instance Overview".to_string(),
                    profile_id,
                    window,
                    cx,
                );
                assert!(
                    catalog_calls.lock().expect("catalog calls").is_empty(),
                    "instance_catalog must not run synchronously on the foreground thread"
                );
                assert_eq!(default_dashboard_calls.load(Ordering::SeqCst), 0);
            });
        });

        window.run_until_parked();

        assert_eq!(default_dashboard_calls.load(Ordering::SeqCst), 1);
        window.update(|_, cx| {
            let state = app_state.read(cx);
            let dashboards = state.dashboards.all_dashboards();
            assert_eq!(dashboards.len(), 1);
            let panels = state.dashboards.panels_for_dashboard(dashboards[0].id);
            assert_eq!(panels.len(), 1);
        });
    }

    #[gpui::test]
    fn open_instance_overview_drops_stale_result_after_reconnect(cx: &mut TestAppContext) {
        let TestWorkspace {
            workspace,
            app_state,
            window,
            profile_id,
            default_dashboard_calls,
            ..
        } = setup_workspace(cx);

        let new_default_dashboard_calls = window.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.open_instance_overview(profile_id, window, cx);
            });
            reconnect_profile(&app_state, profile_id, cx)
        });

        window.run_until_parked();

        assert_eq!(default_dashboard_calls.load(Ordering::SeqCst), 1);
        assert_eq!(new_default_dashboard_calls.load(Ordering::SeqCst), 0);
        window.update(|_, cx| {
            let tab_id = workspace
                .read(cx)
                .tab_manager
                .read(cx)
                .find_by_key(&DocumentKey::InstanceOverview { profile_id }, cx);
            assert!(
                tab_id.is_none(),
                "stale catalog descriptor from an earlier connection must not open an overview tab"
            );
        });
    }

    #[gpui::test]
    fn save_overview_as_editable_drops_stale_result_after_reconnect(cx: &mut TestAppContext) {
        let TestWorkspace {
            workspace,
            app_state,
            window,
            profile_id,
            default_dashboard_calls,
            ..
        } = setup_workspace(cx);

        let new_default_dashboard_calls = window.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.save_overview_as_editable(
                    "Instance Overview".to_string(),
                    profile_id,
                    window,
                    cx,
                );
            });
            reconnect_profile(&app_state, profile_id, cx)
        });

        window.run_until_parked();

        assert_eq!(default_dashboard_calls.load(Ordering::SeqCst), 1);
        assert_eq!(new_default_dashboard_calls.load(Ordering::SeqCst), 0);
        window.update(|_, cx| {
            let dashboards = app_state.read(cx).dashboards.all_dashboards();
            assert!(
                dashboards.is_empty(),
                "stale catalog descriptor from an earlier connection must not persist a dashboard"
            );
        });
    }
}
