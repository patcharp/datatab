use super::*;
use crate::ui::labels::{
    charts_add_panel_failed_message, charts_add_panels_failed_message,
    charts_cannot_open_chart_message, charts_create_dashboard_failed_message,
    charts_delete_chart_failed_message, charts_delete_dashboard_failed_message,
    charts_duplicate_chart_failed_message, charts_duplicate_dashboard_failed_message,
    charts_persist_chart_for_panel_failed_message, charts_rename_failed_message,
};

impl Workspace {
    /// Opens a new `ChartDocument` seeded with the given query.
    ///
    /// Called when the user selects "Chart this query" from a data grid context menu.
    pub(in crate::ui::views::workspace) fn open_chart_from_query(
        &mut self,
        query: String,
        connection_id: Option<uuid::Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doc = cx.new(|cx| {
            crate::ui::document::ChartDocument::new(
                connection_id,
                query,
                self.app_state.clone(),
                window,
                cx,
            )
        });
        let pane = crate::ui::document::ChartDocument::into_pane(doc, cx);

        self.tab_manager.update(cx, |mgr, cx| {
            mgr.open(Tab::Pane(Box::new(pane)), cx);
        });

        self.set_focus(FocusTarget::Document, window, cx);
    }

    /// Builds palette items for all saved charts in the current profile (or all profiles).
    ///
    /// Used by the "Open chart..." command to show a fuzzy-searchable chart list.
    pub(in crate::ui::views::workspace) fn build_saved_chart_palette_items(
        &self,
        cx: &Context<Self>,
    ) -> Vec<PaletteItem> {
        let app_state = self.app_state.read(cx);
        let active_profile_id = app_state.active_connection_id();

        let charts: Vec<datatab_components::SavedChart> = app_state
            .saved_charts
            .all_charts()
            .iter()
            .filter(|chart| {
                // Show charts for the active profile, or all charts when no profile is active.
                active_profile_id
                    .map(|id| chart.profile_id == id)
                    .unwrap_or(true)
            })
            .cloned()
            .collect();

        let profiles = app_state.profiles();

        charts
            .into_iter()
            .map(|chart| {
                let profile_name = profiles
                    .iter()
                    .find(|p| p.id == chart.profile_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| datatab_i18n::t!("charts.orphaned_profile_label"));

                PaletteItem::SavedChart {
                    id: chart.id,
                    name: chart.name.clone(),
                    profile_name,
                    profile_id: chart.profile_id,
                    is_collection_source: chart.is_collection_source(),
                }
            })
            .collect()
    }

    /// Opens a `ChartDocument` for the given saved chart ID.
    ///
    /// If a tab for this chart is already open, focuses it instead.
    pub(in crate::ui::views::workspace) fn open_saved_chart(
        &mut self,
        chart_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Focus existing tab if the chart is already open.
        let existing_id = self.tab_manager.read(cx).find_by_key(
            &crate::ui::document::DocumentKey::Chart {
                saved_chart_id: chart_id,
            },
            cx,
        );

        if let Some(id) = existing_id {
            self.tab_manager.update(cx, |mgr, cx| {
                mgr.activate(id, cx);
            });
            self.set_focus(FocusTarget::Document, window, cx);
            return;
        }

        let saved_chart = self
            .app_state
            .read(cx)
            .saved_charts
            .all_charts()
            .iter()
            .find(|c| c.id == chart_id)
            .cloned();

        let Some(chart) = saved_chart else {
            Toast::error(datatab_i18n::t!("charts.toast.chart_not_found"))
                .meta_right(now_hms())
                .push(cx);
            return;
        };

        // Route based on source type.
        match &chart.source {
            datatab_components::saved_chart::SavedChartSource::Collection {
                collection_ref,
                ..
            } => {
                // Collection charts open as a DataDocument tab in Chart mode.
                // The DataDocument's auto-select logic will switch to Chart mode
                // after the data loads (TimeSeries shape triggers auto-select).
                let collection = collection_ref.clone();
                self.open_collection_document(
                    chart.profile_id,
                    collection,
                    TabPlacement::ReuseExisting,
                    window,
                    cx,
                );
            }
            datatab_components::saved_chart::SavedChartSource::Query { .. }
            | datatab_components::saved_chart::SavedChartSource::InstanceMetric { .. } => {
                // Validate before allocating an entity — from_saved checks the source variant.
                let validation = crate::ui::document::ChartDocument::validate_saved_source(&chart);
                if let Err(e) = validation {
                    report_error(
                        UserFacingError::new(
                            ErrorKind::Storage,
                            charts_cannot_open_chart_message(e),
                        ),
                        cx,
                    );
                    return;
                }

                let app_state = self.app_state.clone();
                #[expect(
                    clippy::expect_used,
                    reason = "`from_saved` returns `Err` only for `Collection` \
                              sources (the `Collection` arm of its `match \
                              &saved.source` in chart_document/mod.rs), and a \
                              `Collection` source never reaches this arm: the \
                              outer `match` above routes it to \
                              `open_collection_document` and returns"
                )]
                let doc = cx.new(|cx| {
                    // from_saved is guaranteed Ok for Query and InstanceMetric sources (validated above).
                    crate::ui::document::ChartDocument::from_saved(&chart, app_state, window, cx)
                        .expect("Query/InstanceMetric source validated before entity creation")
                });

                let pane = crate::ui::document::ChartDocument::into_pane(doc, cx);
                self.tab_manager.update(cx, |mgr, cx| {
                    mgr.open(Tab::Pane(Box::new(pane)), cx);
                });
                self.set_focus(FocusTarget::Document, window, cx);
            }
        }
    }

    /// Opens a `DashboardDocument` for the given dashboard ID.
    ///
    /// If a tab for this dashboard is already open, focuses it instead of
    /// creating a duplicate. Panel slots are built from the dashboard's
    /// persisted panel list: for each panel, if the referenced `SavedChart`
    /// exists and has a `Query` source, a live `ChartDocument` entity is
    /// created (`Loaded`); otherwise the slot is `Orphan`.
    ///
    /// This method does not inspect `driver_id`; capability gating for the
    /// import affordance is handled separately in the import flow.
    #[allow(dead_code)]
    pub(in crate::ui::views::workspace) fn open_dashboard(
        &mut self,
        dashboard_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Dedup: focus the existing tab if the dashboard is already open.
        let existing_id = self.tab_manager.read(cx).find_by_key(
            &crate::ui::document::DocumentKey::Dashboard { dashboard_id },
            cx,
        );

        if let Some(id) = existing_id {
            self.tab_manager.update(cx, |mgr, cx| {
                mgr.activate(id, cx);
            });
            self.set_focus(FocusTarget::Document, window, cx);
            return;
        }

        // Look up the dashboard metadata.
        let dashboard_meta = self
            .app_state
            .read(cx)
            .dashboards
            .dashboard_by_id(dashboard_id)
            .cloned();

        let Some(dashboard) = dashboard_meta else {
            Toast::error(datatab_i18n::t!("charts.toast.dashboard_not_found"))
                .meta_right(now_hms())
                .push(cx);
            return;
        };

        // Build panel slots from persisted panels.
        // Dedup panels by saved_chart_id when reading the persisted set.
        // Past bugs could persist two rows pointing to the same saved chart
        // (creating visible duplicates with identical data); this guard
        // keeps an already-affected dashboard usable without forcing the
        // user to delete and re-create panels manually.
        let mut panels: Vec<datatab_ui_base::DashboardPanel> = {
            let raw = self
                .app_state
                .read(cx)
                .dashboards
                .panels_for_dashboard(dashboard_id)
                .to_vec();
            let mut seen: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
            // Dividers don't dedup against saved_chart_id (they have none); pass
            // them through unconditionally. Chart panels dedup on saved_chart_id
            // so stale "two rows for the same chart" never materialises twice.
            raw.into_iter()
                .filter(|p| match p.saved_chart_id() {
                    Some(id) => seen.insert(id),
                    None => true,
                })
                .collect()
        };

        // One-shot rescale of pre-12-col data. Old dashboards persisted with
        // `grid_columns < 12` are widened in place so subsequent loads see
        // canonical 12-col-native coordinates. No SQL migration is required —
        // the new positions are written back through `update_panel_position`.
        use crate::ui::document::dashboard::{DASHBOARD_GRID_COLUMNS, rescale_panel_to_12_cols};
        if dashboard.grid_columns < DASHBOARD_GRID_COLUMNS && !panels.is_empty() {
            for panel in panels.iter_mut() {
                let (new_col, new_w) = rescale_panel_to_12_cols(
                    panel.grid_column,
                    panel.grid_width,
                    dashboard.grid_columns,
                );
                if new_col != panel.grid_column || new_w != panel.grid_width {
                    let dashboard_id_local = dashboard_id;
                    let panel_index = panel.panel_index;
                    let new_row = panel.grid_row;
                    let new_h = panel.grid_height;
                    let result = self.app_state.update(cx, |state, _cx| {
                        state.dashboards.update_panel_position(
                            dashboard_id_local,
                            panel_index,
                            new_col,
                            new_row,
                            new_w,
                            new_h,
                        )
                    });
                    if let Err(e) = result {
                        let message = e.to_string();
                        self.app_state.update(cx, |state, _cx| {
                            state.record_storage_failure(
                                datatab_core::observability::actions::CONFIG_UPDATE,
                                "dashboard_panel",
                                format!("{dashboard_id_local}#{panel_index}"),
                                datatab_i18n::t!("charts.error.rescale_failed"),
                                message,
                            );
                        });
                    }
                    panel.grid_column = new_col;
                    panel.grid_width = new_w;
                }
            }
        }

        let app_state = self.app_state.clone();
        let doc = cx.new(|cx| {
            use crate::ui::document::DashboardDocument;
            use crate::ui::document::DashboardPanelSlot;
            use datatab_components::common::time_range::{TimeRange, TimeRangePanel};

            // Build panel slots: Loaded when a Query/InstanceMetric-source chart exists,
            // Orphan otherwise. Grid position is carried on every slot so
            // the render can sort and size panels correctly.
            let panel_slots: Vec<DashboardPanelSlot> =
                panels
                    .iter()
                    .map(|panel| {
                        use crate::ui::document::dashboard::PanelGridPos;

                        let grid_pos = PanelGridPos {
                            grid_row: panel.grid_row,
                            grid_column: panel.grid_column,
                            grid_width: panel.grid_width,
                            grid_height: panel.grid_height,
                        };

                        // Divider panels render directly without resolving a chart.
                        if let datatab_ui_base::DashboardPanelKind::Divider { markdown } =
                            &panel.kind
                        {
                            return DashboardPanelSlot::Divider {
                                markdown: markdown.clone(),
                                grid_pos,
                            };
                        }

                        // Inspector panels are instantiated as live entities.
                        // `profile_id` comes from the dashboard's profile association.
                        if let datatab_ui_base::DashboardPanelKind::Inspector { metric_id } =
                            &panel.kind
                        {
                            use crate::ui::document::InspectorPanel;
                            if let Some(prof_id) = dashboard.profile_id {
                                let metric_id = metric_id.clone();
                                let app_state_inner = app_state.clone();
                                let inspector_entity = cx.new(|cx| {
                                    InspectorPanel::new(prof_id, metric_id, app_state_inner, cx)
                                });
                                inspector_entity.update(cx, |p, _cx| p.defer_initial_exec());
                                return DashboardPanelSlot::Inspector {
                                    entity: inspector_entity,
                                    grid_pos,
                                    title_override: panel.title_override.clone(),
                                };
                            }
                        }

                        let saved_chart_id = panel.saved_chart_id().unwrap_or_else(uuid::Uuid::nil);

                        let chart = app_state
                            .read(cx)
                            .saved_charts
                            .all_charts()
                            .iter()
                            .find(|c| c.id == saved_chart_id)
                            .cloned();

                        match chart {
                            Some(saved_chart)
                                if matches!(
                                saved_chart.source,
                                datatab_components::saved_chart::SavedChartSource::Query { .. }
                                    | datatab_components::saved_chart::SavedChartSource::InstanceMetric {
                                        ..
                                    }
                            ) =>
                            {
                                let app_state_inner = app_state.clone();
                                #[expect(
                                    clippy::expect_used,
                                    reason = "the match guard above admits the Query and \
                                              InstanceMetric sources, \
                                              and `from_saved` returns `Err` only \
                                              for the Collection source (the \
                                              `Collection` arm of its `match \
                                              &saved.source` in \
                                              chart_document/mod.rs), so \
                                              `from_saved` returns `Ok` here"
                                )]
                                let panel_entity = cx.new(|cx| {
                                    let mut doc = crate::ui::document::ChartDocument::from_saved(
                                        &saved_chart,
                                        app_state_inner,
                                        window,
                                        cx,
                                    )
                                    .expect("Query/InstanceMetric source validated before entity creation");
                                    // Mark embedded so the chart's own chrome
                                    // (title/Run/Save segments + internal
                                    // toolbar row) is suppressed; the
                                    // dashboard panel provides the chrome.
                                    doc.set_embedded(true, cx);
                                    doc
                                });
                                DashboardPanelSlot::Loaded {
                                    panel: panel_entity,
                                    grid_pos,
                                    title_override: panel.title_override.clone(),
                                }
                            }
                            _ => DashboardPanelSlot::Orphan {
                                saved_chart_id,
                                grid_pos,
                            },
                        }
                    })
                    .collect();

            // Build the shared time-range panel using the persisted preset when
            // available; fall back to Last24Hours (index 3) when the dashboard
            // has no stored preset.
            use datatab_components::saved_chart::TimeRangePreset;
            let (preset_range, preset_index) = match dashboard.shared_time_range_preset {
                Some(TimeRangePreset::Last15min) => (TimeRange::Last15min, Some(0usize)),
                Some(TimeRangePreset::LastHour) => (TimeRange::LastHour, Some(1)),
                Some(TimeRangePreset::Last6Hours) => (TimeRange::Last6Hours, Some(2)),
                Some(TimeRangePreset::Last24Hours) | None => (TimeRange::Last24Hours, Some(3)),
                Some(TimeRangePreset::Last7Days) => (TimeRange::Last7Days, Some(4)),
            };
            let preset_placeholder = TimeRangePanel::preset_label(preset_range);
            let shared_time_range =
                cx.new(|cx| TimeRangePanel::new(preset_placeholder, preset_index, window, cx));

            DashboardDocument::new(
                dashboard_id,
                dashboard.name.clone(),
                panel_slots,
                shared_time_range,
                dashboard.shared_time_range_preset,
                dashboard.shared_refresh_policy,
                false,
                app_state.clone(),
                cx,
            )
        });

        let pane = crate::ui::document::DashboardDocument::into_pane(doc, cx);
        self.tab_manager.update(cx, |mgr, cx| {
            mgr.open(Tab::Pane(Box::new(pane)), cx);
        });
        self.set_focus(FocusTarget::Document, window, cx);
    }

    // --- Phase P stubs: dashboard and saved-chart workspace actions ---
    // Full implementations arrive in Phase P; these stubs wire the Phase N
    // sidebar-event routing so the crate compiles before modals exist.

    /// Open the "New Dashboard" creation modal for the given profile.
    ///
    /// Called when the user selects "New dashboard…" from the sidebar context
    /// menu on a DashboardsFolder node.
    pub(in crate::ui::views::workspace) fn create_dashboard_from_sidebar(
        &mut self,
        profile_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal_create_dashboard.update(cx, |modal, cx| {
            modal.open(CreateDashboardRequest { profile_id }, window, cx);
        });
    }

    /// Open the "New dashboard…" modal from the command palette.
    ///
    /// Uses the active connection's profile as the target profile. If no
    /// connection is active but profiles exist, uses the first profile.
    /// Shows a toast if no profiles are configured.
    pub(in crate::ui::views::workspace) fn create_dashboard_from_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile_id = self
            .app_state
            .read(cx)
            .active_connection()
            .map(|c| c.profile.id)
            .or_else(|| self.app_state.read(cx).profiles().first().map(|p| p.id));

        match profile_id {
            Some(profile_id) => {
                self.modal_create_dashboard.update(cx, |modal, cx| {
                    modal.open(CreateDashboardRequest { profile_id }, window, cx);
                });
            }
            None => {
                Toast::warning(datatab_i18n::t!("charts.toast.add_profile_first"))
                    .meta_right(now_hms())
                    .push(cx);
            }
        }
    }

    /// Called when `ModalCreateDashboard` emits `Confirmed`.
    ///
    /// Creates the dashboard in the manager, triggers a sidebar rebuild, and
    /// opens the new dashboard tab.
    pub(in crate::ui::views::workspace) fn on_create_dashboard_confirmed(
        &mut self,
        profile_id: uuid::Uuid,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.app_state.update(cx, |state, _cx| {
            state.dashboards.create_dashboard(
                name.clone(),
                None,
                profile_id,
                None,
                datatab_components::saved_chart::SavedChartRefreshPolicy::Off,
            )
        });

        match result {
            Ok(dashboard_id) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
                self.open_dashboard(dashboard_id, window, cx);
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(
                        ErrorKind::Storage,
                        charts_create_dashboard_failed_message(e),
                    ),
                    cx,
                );
            }
        }
    }

    /// Open the rename modal for a dashboard.
    pub(in crate::ui::views::workspace) fn rename_dashboard(
        &mut self,
        dashboard_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_name = self
            .app_state
            .read(cx)
            .dashboards
            .dashboard_by_id(dashboard_id)
            .map(|d| d.name.clone())
            .unwrap_or_default();

        self.modal_rename_item.update(cx, |modal, cx| {
            modal.open(
                RenameItemRequest {
                    target: RenameTarget::Dashboard { dashboard_id },
                    current_name,
                },
                window,
                cx,
            );
        });
    }

    /// Delete a dashboard after confirmation.
    ///
    /// Opens the delete confirmation modal. On confirm, the tab is closed
    /// before the row is removed from the repository (see
    /// `on_delete_dashboard_confirmed`).
    pub(in crate::ui::views::workspace) fn delete_dashboard(
        &mut self,
        dashboard_id: uuid::Uuid,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dashboard_name = self
            .app_state
            .read(cx)
            .dashboards
            .dashboard_by_id(dashboard_id)
            .map(|d| d.name.clone())
            .unwrap_or_default();

        self.modal_delete_dashboard.update(cx, |modal, cx| {
            modal.open(
                DeleteConfirmRequest::Dashboard {
                    dashboard_id,
                    dashboard_name,
                },
                cx,
            );
        });
    }

    /// Called when the dashboard delete confirmation emits `Confirmed`.
    ///
    /// Closes the open tab first, then deletes the dashboard row and panels,
    /// then triggers a sidebar rebuild.
    pub(in crate::ui::views::workspace) fn on_delete_dashboard_confirmed(
        &mut self,
        dashboard_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Close the open tab before deleting the row so the UI never references
        // a deleted entity. The removal goes through the funnel's own step rather
        // than the tab manager: a dashboard holds no unsaved edits to ask about,
        // and it must close even if a close policy were ever added to it, because
        // the row it renders is about to stop existing.
        let key = crate::ui::document::DocumentKey::Dashboard { dashboard_id };
        if let Some(doc_id) = self.tab_manager.read(cx).find_by_key(&key, cx) {
            self.close_tab_now(doc_id, window, cx);
        }

        let result = self.app_state.update(cx, |state, _cx| {
            state.dashboards.delete_dashboard(dashboard_id)
        });

        match result {
            Ok(()) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(
                        ErrorKind::Storage,
                        charts_delete_dashboard_failed_message(e),
                    ),
                    cx,
                );
            }
        }
    }

    /// Duplicate a dashboard without a modal (immediate action).
    pub(in crate::ui::views::workspace) fn duplicate_dashboard(
        &mut self,
        dashboard_id: uuid::Uuid,
        cx: &mut Context<Self>,
    ) {
        let result = self.app_state.update(cx, |state, _cx| {
            state.dashboards.duplicate_dashboard(dashboard_id)
        });

        match result {
            Ok(_new_id) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(
                        ErrorKind::Storage,
                        charts_duplicate_dashboard_failed_message(e),
                    ),
                    cx,
                );
            }
        }
    }

    /// Open the rename modal for a saved chart.
    pub(in crate::ui::views::workspace) fn rename_saved_chart(
        &mut self,
        chart_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_name = self
            .app_state
            .read(cx)
            .saved_charts
            .all_charts()
            .iter()
            .find(|c| c.id == chart_id)
            .map(|c| c.name.clone())
            .unwrap_or_default();

        self.modal_rename_item.update(cx, |modal, cx| {
            modal.open(
                RenameItemRequest {
                    target: RenameTarget::SavedChart { chart_id },
                    current_name,
                },
                window,
                cx,
            );
        });
    }

    /// Called when `ModalRenameItem` emits `Confirmed`.
    ///
    /// Dispatches to the appropriate manager based on `RenameTarget`.
    pub(in crate::ui::views::workspace) fn on_rename_item_confirmed(
        &mut self,
        target: RenameTarget,
        new_name: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match &target {
            RenameTarget::Dashboard { dashboard_id } => {
                let id = *dashboard_id;
                self.app_state.update(cx, |state, _cx| {
                    state.dashboards.rename_dashboard(id, new_name)
                })
            }
            RenameTarget::SavedChart { chart_id } => {
                let id = *chart_id;
                self.app_state.update(cx, |state, _cx| {
                    state.saved_charts.rename_chart(id, new_name)
                })
            }
        };

        match result {
            Ok(()) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(ErrorKind::Storage, charts_rename_failed_message(e)),
                    cx,
                );
            }
        }
    }

    /// Delete a saved chart after confirmation.
    ///
    /// Pre-queries `find_dashboards_referencing_chart` to populate the
    /// orphan-warning list in the confirmation modal.
    pub(in crate::ui::views::workspace) fn delete_saved_chart(
        &mut self,
        chart_id: uuid::Uuid,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chart_name = self
            .app_state
            .read(cx)
            .saved_charts
            .all_charts()
            .iter()
            .find(|c| c.id == chart_id)
            .map(|c| c.name.clone())
            .unwrap_or_default();

        // Build the referencing-dashboard list for the orphan-warning block.
        // dashboards_repo() is fallible; degrade gracefully on open failure while logging.
        let referencing_ids = self
            .app_state
            .read(cx)
            .storage_runtime()
            .dashboards_repo()
            .map_err(|e| {
                log::warn!("Failed to open dashboards repo for orphan check: {e:?}");
                e
            })
            .ok()
            .and_then(|r| {
                r.find_dashboards_referencing_chart(chart_id)
                    .map_err(|e| {
                        log::warn!(
                            "Failed to query dashboards referencing chart {chart_id}: {e:?}"
                        );
                        e
                    })
                    .ok()
            })
            .unwrap_or_default();

        let referencing_dashboards: Vec<(uuid::Uuid, String)> = referencing_ids
            .into_iter()
            .filter_map(|did| {
                self.app_state
                    .read(cx)
                    .dashboards
                    .dashboard_by_id(did)
                    .map(|d| (did, d.name.clone()))
            })
            .collect();

        self.modal_delete_saved_chart.update(cx, |modal, cx| {
            modal.open(
                DeleteConfirmRequest::SavedChart {
                    chart_id,
                    chart_name,
                    referencing_dashboards,
                },
                cx,
            );
        });
    }

    /// Called when the saved chart delete confirmation emits `Confirmed`.
    pub(in crate::ui::views::workspace) fn on_delete_saved_chart_confirmed(
        &mut self,
        chart_id: uuid::Uuid,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self
            .app_state
            .update(cx, |state, _cx| state.saved_charts.delete_chart(chart_id));

        match result {
            Ok(()) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(ErrorKind::Storage, charts_delete_chart_failed_message(e)),
                    cx,
                );
            }
        }
    }

    /// Duplicate a saved chart without a modal (immediate action).
    pub(in crate::ui::views::workspace) fn duplicate_saved_chart(
        &mut self,
        chart_id: uuid::Uuid,
        cx: &mut Context<Self>,
    ) {
        let result = self.app_state.update(cx, |state, _cx| {
            state.saved_charts.duplicate_chart(chart_id)
        });

        match result {
            Ok(_new_id) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(
                        ErrorKind::Storage,
                        charts_duplicate_chart_failed_message(e),
                    ),
                    cx,
                );
            }
        }
    }

    /// Open the "Add Panel" picker for a specific dashboard.
    pub(in crate::ui::views::workspace) fn open_add_panel_picker(
        &mut self,
        dashboard_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile_id_opt = self
            .app_state
            .read(cx)
            .dashboards
            .dashboard_by_id(dashboard_id)
            .and_then(|d| d.profile_id);

        let candidates: Vec<datatab_components::saved_chart::SavedChart> =
            if let Some(pid) = profile_id_opt {
                self.app_state
                    .read(cx)
                    .saved_charts
                    .charts_for_profile(pid)
                    .into_iter()
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

        self.modal_add_panel.update(cx, |modal, cx| {
            modal.open(
                AddPanelRequest {
                    dashboard_id,
                    profile_id: profile_id_opt.unwrap_or_else(uuid::Uuid::nil),
                    candidates,
                },
                window,
                cx,
            );
        });
    }

    /// Build a new SavedChart from a user-typed query, persist it, and append
    /// it as a panel to the target dashboard.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::ui::views::workspace) fn on_create_panel_from_query(
        &mut self,
        dashboard_id: uuid::Uuid,
        profile_id: uuid::Uuid,
        name: String,
        query: String,
        chart_kind: datatab_components::chart::ChartKind,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use datatab_components::chart::{AxisKind, AxisSpec, BindingSpec, ChartSpec, YScale};
        use datatab_components::saved_chart::SavedChart;
        use datatab_ui_base::DashboardPanelDraft;

        let placeholder_spec = ChartSpec {
            kind: chart_kind,
            x_axis: AxisSpec {
                column_index: 0,
                label: String::new(),
                kind: AxisKind::Time,
                unit: None,
            },
            series: Vec::new(),
            legend_visible: false,
            decimation_threshold: 10_000,
            binding: BindingSpec::default(),
            track_source_indices: false,
            y_scale: YScale::Linear,
        };

        let chart = SavedChart::new_query(
            name.clone(),
            profile_id,
            query,
            placeholder_spec,
            BindingSpec::default(),
        );
        let chart_id = chart.id;

        let append_result = self.app_state.update(cx, |state, _cx| {
            if let Err(e) = state.saved_charts.upsert(chart) {
                state.record_storage_failure(
                    datatab_core::observability::actions::CONFIG_CREATE,
                    "saved_chart",
                    chart_id.to_string(),
                    charts_persist_chart_for_panel_failed_message(&name),
                    e.to_string(),
                );
                return Err(e);
            }
            state.dashboards.append_panels(
                dashboard_id,
                vec![DashboardPanelDraft::Chart {
                    saved_chart_id: chart_id,
                    layout: None,
                }],
            )
        });

        match append_result {
            Ok(()) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(ErrorKind::Storage, charts_add_panel_failed_message(e)),
                    cx,
                );
            }
        }
    }

    /// Called when `ModalAddPanelPicker` emits `Confirmed`.
    pub(in crate::ui::views::workspace) fn on_add_panels_confirmed(
        &mut self,
        dashboard_id: uuid::Uuid,
        chart_ids: Vec<uuid::Uuid>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use datatab_ui_base::DashboardPanelDraft;

        let drafts: Vec<DashboardPanelDraft> = chart_ids
            .into_iter()
            .map(|saved_chart_id| DashboardPanelDraft::Chart {
                saved_chart_id,
                layout: None,
            })
            .collect();

        let result = self.app_state.update(cx, |state, _cx| {
            state.dashboards.append_panels(dashboard_id, drafts)
        });

        match result {
            Ok(()) => {
                self.app_state.update(cx, |_state, cx| {
                    cx.emit(AppStateChanged);
                });
            }
            Err(e) => {
                report_error(
                    UserFacingError::new(ErrorKind::Storage, charts_add_panels_failed_message(e)),
                    cx,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    const CHARTS_CATALOG_KEYS: &[&str] = &[
        "charts.orphaned_profile_label",
        "charts.toast.chart_not_found",
        "charts.toast.dashboard_not_found",
        "charts.toast.add_profile_first",
        "charts.error.cannot_open_chart",
        "charts.error.rescale_failed",
        "charts.error.create_dashboard_failed",
        "charts.error.delete_dashboard_failed",
        "charts.error.duplicate_dashboard_failed",
        "charts.error.rename_failed",
        "charts.error.delete_chart_failed",
        "charts.error.duplicate_chart_failed",
        "charts.error.persist_chart_for_panel_failed",
        "charts.error.add_panel_failed",
        "charts.error.add_panels_failed",
    ];

    #[::core::prelude::v1::test]
    fn charts_keys_resolve() {
        for key in CHARTS_CATALOG_KEYS {
            let value = datatab_i18n::t!(key, locale = "en");

            assert!(!value.is_empty(), "key {key} resolved empty");
            assert_ne!(value, *key, "key {key} did not resolve");
            assert_ne!(
                value,
                format!("en.{key}"),
                "key {key} fell back to the raw en-qualified form"
            );
        }
    }

    #[::core::prelude::v1::test]
    fn charts_dashboard_not_found_english_text() {
        let english = datatab_i18n::t!("charts.toast.dashboard_not_found", locale = "en");

        assert_eq!(english, "Dashboard not found");
    }
}
