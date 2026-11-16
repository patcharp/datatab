use super::*;
use datatab_core::{SchemaDropTarget, SchemaObjectKind};

impl Sidebar {
    pub fn expand_collapse(&mut self, cx: &mut Context<Self>) {
        let tree = self.active_tree_state().clone();
        let entry = tree.read(cx).selected_entry().cloned();
        if let Some(entry) = entry
            && entry.is_folder()
        {
            let item_id = entry.item().id.to_string();
            let currently_expanded = entry.is_expanded();
            self.set_expanded(&item_id, !currently_expanded, cx);
        }
    }

    pub fn collapse(&mut self, cx: &mut Context<Self>) {
        let tree = self.active_tree_state().clone();
        let entry = tree.read(cx).selected_entry().cloned();
        if let Some(entry) = entry
            && entry.is_folder()
            && entry.is_expanded()
        {
            let item_id = entry.item().id.to_string();
            self.set_expanded(&item_id, false, cx);
        }
    }

    /// Expands the selected row. A disconnected connection has nothing to
    /// expand yet, so it is connected instead, as its chevron does, and opens
    /// once the connection is up.
    pub fn expand(&mut self, cx: &mut Context<Self>) {
        let tree = self.active_tree_state().clone();
        let entry = tree.read(cx).selected_entry().cloned();

        if let Some(entry) = &entry
            && self.connect_if_disconnected_profile(entry.item().id.as_ref(), cx)
        {
            return;
        }

        if let Some(entry) = entry
            && entry.is_folder()
            && !entry.is_expanded()
        {
            let item_id = entry.item().id.to_string();
            self.set_expanded(&item_id, true, cx);
        }
    }

    /// Starts connecting `item_id` when it is a connection row that is not
    /// connected, and reports whether it did. Expanding such a row, by its
    /// chevron or by `l`, connects it; the connected row then opens.
    pub(super) fn connect_if_disconnected_profile(
        &mut self,
        item_id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(SchemaNodeId::Profile { profile_id }) = parse_node_id(item_id) else {
            return false;
        };

        if self
            .app_state
            .read(cx)
            .connections()
            .contains_key(&profile_id)
        {
            return false;
        }

        self.connect_to_profile(profile_id, cx);
        true
    }

    pub(super) fn set_expanded(&mut self, item_id: &str, expanded: bool, cx: &mut Context<Self>) {
        if expanded && !self.trigger_expansion_fetch(item_id, cx) {
            return;
        }

        let parsed = parse_node_id(item_id);

        if let Some(SchemaNodeId::ConnectionFolder { node_id }) = parsed {
            self.app_state.update(cx, |state, _cx| {
                state.set_folder_collapsed(node_id, !expanded);
            });
        }

        self.expansion_overrides
            .insert(item_id.to_string(), expanded);

        // Script folders live in `scripts_tree_state`, which the connections
        // rebuild path ignores. Without this, the override is recorded but
        // never propagated, leaving the folder visually stuck open.
        if matches!(
            parsed,
            Some(SchemaNodeId::ScriptsFolder { .. })
                | Some(SchemaNodeId::ScriptFile { .. })
                | Some(SchemaNodeId::ScriptsRoot { .. })
        ) {
            self.refresh_scripts_tree(cx);
        } else {
            self.rebuild_tree_with_overrides(cx);
        }
    }

    /// Starts any background fetches required when a node is expanded.
    /// Returns `false` if expansion should be blocked (e.g. fetch preparation failed).
    /// Does not modify `expansion_overrides` or rebuild the tree.
    pub(super) fn trigger_expansion_fetch(
        &mut self,
        item_id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        self.inline_list_fetches.clear();
        let parsed = parse_node_id(item_id);

        if let Some(SchemaNodeId::Database { profile_id, name }) = &parsed
            && self
                .app_state
                .read(cx)
                .connections()
                .get(profile_id)
                .is_some_and(|connected| {
                    connected.connection.schema_loading_strategy()
                        == datatab_core::SchemaLoadingStrategy::LazyPerDatabase
                })
        {
            self.handle_lazy_database_click(*profile_id, name, cx);
        }

        if matches!(parsed, Some(SchemaNodeId::Table { .. })) {
            let pending = PendingAction::ViewSchema {
                item_id: item_id.to_string(),
            };
            if matches!(
                self.ensure_table_details(item_id, pending, cx),
                TableDetailsStatus::NotFound
            ) {
                return false;
            }
        }

        if let Some(SchemaNodeId::Collection { .. }) = &parsed {
            let pending = PendingAction::ExpandCollection {
                item_id: item_id.to_string(),
            };

            if matches!(
                self.ensure_table_details(item_id, pending, cx),
                TableDetailsStatus::NotFound
            ) {
                return false;
            }
        }

        if let Some(SchemaNodeId::TypesFolder {
            profile_id,
            database,
            schema,
        }) = &parsed
        {
            let needs_fetch =
                self.app_state
                    .read(cx)
                    .needs_schema_types(*profile_id, database, Some(schema));

            if needs_fetch {
                let pending = PendingAction::ExpandTypesFolder {
                    item_id: item_id.to_string(),
                };
                if !self.spawn_fetch_schema_types(*profile_id, database, Some(schema), pending, cx)
                {
                    return false;
                }
            }
        }

        if let Some(SchemaNodeId::SchemaIndexesFolder {
            profile_id,
            database,
            schema,
        }) = &parsed
        {
            let needs_fetch =
                self.app_state
                    .read(cx)
                    .needs_schema_indexes(*profile_id, database, Some(schema));

            if needs_fetch {
                let pending = PendingAction::ExpandSchemaIndexesFolder {
                    item_id: item_id.to_string(),
                };
                if !self.spawn_fetch_schema_indexes(
                    *profile_id,
                    database,
                    Some(schema),
                    pending,
                    cx,
                ) {
                    return false;
                }
            }
        }

        if let Some(SchemaNodeId::SchemaForeignKeysFolder {
            profile_id,
            database,
            schema,
        }) = &parsed
        {
            let needs_fetch = self.app_state.read(cx).needs_schema_foreign_keys(
                *profile_id,
                database,
                Some(schema),
            );

            if needs_fetch {
                let pending = PendingAction::ExpandSchemaForeignKeysFolder {
                    item_id: item_id.to_string(),
                };
                if !self.spawn_fetch_schema_foreign_keys(
                    *profile_id,
                    database,
                    Some(schema),
                    pending,
                    cx,
                ) {
                    return false;
                }
            }
        }

        if let Some(SchemaNodeId::RoutinesFolder {
            profile_id,
            database,
            schema,
        }) = &parsed
        {
            let needs_fetch =
                self.app_state
                    .read(cx)
                    .needs_schema_routines(*profile_id, database, Some(schema));

            if needs_fetch {
                let pending = PendingAction::ExpandSchemaRoutinesFolder {
                    item_id: item_id.to_string(),
                };
                if !self.spawn_fetch_schema_routines(
                    *profile_id,
                    database,
                    Some(schema),
                    pending,
                    cx,
                ) {
                    return false;
                }
            }
        }

        if let Some(SchemaNodeId::InstanceMetricsFolder { profile_id }) = &parsed {
            self.spawn_fetch_instance_catalog(*profile_id, cx);
        }

        if let Some(SchemaNodeId::InstanceInspectorsFolder { profile_id }) = &parsed {
            self.spawn_fetch_instance_catalog(*profile_id, cx);
        }

        if let Some(SchemaNodeId::RootSection { profile_id, .. }) = &parsed {
            self.spawn_fetch_root_sections(*profile_id, cx);
        }

        if let Some(SchemaNodeId::Profile { profile_id }) = &parsed {
            if self.profile_has_root_sections(*profile_id, cx) {
                self.spawn_fetch_root_sections(*profile_id, cx);
            } else {
                let key = datatab_ui_base::object_tree::ObjectTreeRequestKey::DatabaseList {
                    profile_id: *profile_id,
                };
                self.app_state.update(cx, |state, cx| {
                    if state.get_database_list(*profile_id).is_none()
                        && !state.object_tree_is_pending(&key)
                        && state.object_tree_outcome(&key).is_none()
                    {
                        state.object_tree_request(key, cx);
                    }
                });
            }
        }

        if matches!(parsed, Some(SchemaNodeId::Database { .. })) {
            self.handle_database_click(item_id, cx);
        }

        true
    }

    /// Fetch instance metric and inspector definitions for a connection if not cached.
    ///
    /// A single background task calls both `list_metrics` and `list_inspectors` and
    /// stores the results in `instance_metrics_cache` / `instance_inspectors_cache`.
    /// Expanding either the `InstanceMetricsFolder` or the `InstanceInspectorsFolder`
    /// triggers this fetch; deduplication ensures at most one in-flight task per profile.
    pub(super) fn spawn_fetch_instance_catalog(
        &mut self,
        profile_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        if self.instance_metrics_cache.contains_key(&profile_id)
            && self.instance_inspectors_cache.contains_key(&profile_id)
        {
            return;
        }

        if self
            .pending_instance_catalog_fetches
            .contains_key(&profile_id)
        {
            return;
        }

        let connection = match self
            .app_state
            .read(cx)
            .connections()
            .get(&profile_id)
            .map(|c| c.connection.clone())
        {
            Some(c) => c,
            None => return,
        };

        let sidebar = cx.entity().clone();

        let background_task = cx.background_executor().spawn(async move {
            let catalog = match connection.instance_catalog() {
                Some(c) => c,
                None => return Err("driver does not expose an instance catalog".to_string()),
            };

            let metrics = catalog.list_metrics().await.map_err(|e| e.to_string())?;
            let inspectors = catalog.list_inspectors().await.map_err(|e| e.to_string())?;
            Ok((metrics, inspectors))
        });

        let task = cx.spawn(async move |_this, cx| {
            let result = background_task.await;
            cx.update(|cx| {
                sidebar.update(cx, |sidebar, cx| {
                    sidebar.pending_instance_catalog_fetches.remove(&profile_id);
                    match result {
                        Ok((metrics, inspectors)) => {
                            sidebar.instance_metrics_cache.insert(profile_id, metrics);
                            sidebar
                                .instance_inspectors_cache
                                .insert(profile_id, inspectors);
                        }
                        Err(msg) => {
                            log::warn!(
                                "Failed to fetch instance catalog for {}: {}",
                                profile_id,
                                msg
                            );
                        }
                    }
                    sidebar.rebuild_tree_with_overrides(cx);
                });
            });
        });

        self.pending_instance_catalog_fetches
            .insert(profile_id, task);
    }

    /// Remove the cached instance catalog entries for a profile so the next
    /// fetch (triggered separately) retrieves fresh data.
    pub(super) fn clear_instance_catalog_cache(&mut self, profile_id: Uuid) {
        self.instance_metrics_cache.remove(&profile_id);
        self.instance_inspectors_cache.remove(&profile_id);
        self.pending_instance_catalog_fetches.remove(&profile_id);
    }

    /// Whether `profile_id`'s tree node is currently expanded: the user's
    /// explicit collapse/expand override when there is one, otherwise the
    /// builder's default (the active connection expands, the rest stay
    /// collapsed).
    fn profile_node_is_expanded(&self, profile_id: Uuid, cx: &App) -> bool {
        let node_id = SchemaNodeId::Profile { profile_id }.to_string();

        self.expansion_overrides
            .get(&node_id)
            .copied()
            .unwrap_or_else(|| self.app_state.read(cx).active_connection_id() == Some(profile_id))
    }

    /// Starts the fetch behind each visible routines or data-types loading row.
    ///
    /// The Simple navigator view lists these objects directly in the schema, so
    /// no folder expansion triggers their fetch. Each row is fetched once until
    /// the next node expansion, so a failed fetch does not retry on every rebuild.
    fn fetch_visible_inline_lists(&mut self, items: &[TreeItem], cx: &mut Context<Self>) {
        fn collect<'a>(items: &'a [TreeItem], found: &mut Vec<&'a str>) {
            for item in items {
                if item.is_expanded() {
                    collect(&item.children, found);
                } else if item.children.is_empty() {
                    found.push(&item.id);
                }
            }
        }

        let mut visible_leaves = Vec::new();
        collect(items, &mut visible_leaves);
        for leaf_id in visible_leaves {
            let parsed = parse_node_id(leaf_id);
            if !matches!(
                parsed,
                Some(
                    SchemaNodeId::RoutinesLoadingFolder { .. }
                        | SchemaNodeId::TypesLoadingFolder { .. }
                )
            ) || !self.inline_list_fetches.insert(leaf_id.to_string())
            {
                continue;
            }
            match parsed {
                Some(SchemaNodeId::RoutinesLoadingFolder {
                    profile_id,
                    database,
                    schema,
                }) => {
                    let item_id = SchemaNodeId::RoutinesFolder {
                        profile_id,
                        database: database.clone(),
                        schema: schema.clone(),
                    }
                    .to_string();
                    self.spawn_fetch_schema_routines(
                        profile_id,
                        &database,
                        Some(&schema),
                        PendingAction::ExpandSchemaRoutinesFolder { item_id },
                        cx,
                    );
                }
                Some(SchemaNodeId::TypesLoadingFolder {
                    profile_id,
                    database,
                    schema,
                }) => {
                    let item_id = SchemaNodeId::TypesFolder {
                        profile_id,
                        database: database.clone(),
                        schema: schema.clone(),
                    }
                    .to_string();
                    self.spawn_fetch_schema_types(
                        profile_id,
                        &database,
                        Some(&schema),
                        PendingAction::ExpandTypesFolder { item_id },
                        cx,
                    );
                }
                _ => {}
            }
        }
    }

    pub(super) fn rebuild_tree_with_overrides(&mut self, cx: &mut Context<Self>) {
        let selected_id = self
            .tree_state
            .read(cx)
            .selected_entry()
            .map(|entry| entry.item().id.to_string());
        self.active_databases = Self::extract_active_databases(self.app_state.read(cx));

        let items = self.build_tree_items_with_overrides(cx);
        self.fetch_visible_inline_lists(&items, cx);
        self.prune_connection_selection(&items);
        self.visible_entry_count = Self::count_visible_entries(&items);
        self.gutter_metadata = compute_gutter_map(&items);

        let selected_index = selected_id
            .as_deref()
            .and_then(|id| Self::find_item_index_in_tree(&items, id, &mut 0));
        self.syncing_expansion = true;
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
            state.set_selected_index(selected_index, cx);
        });
        self.syncing_expansion = false;
        cx.notify();
    }

    pub(super) fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let selected_id = self
            .tree_state
            .read(cx)
            .selected_entry()
            .map(|entry| entry.item().id.to_string());
        self.active_databases = Self::extract_active_databases(self.app_state.read(cx));

        // Evict catalog cache entries for profiles that are no longer
        // connected. This handles the edge case where a disconnect happens
        // via a code path that does not call `clear_instance_catalog_cache`
        // directly (e.g. connection error, or reconnect to a different account
        // on the same profile UUID).
        let connected_profile_ids: std::collections::HashSet<Uuid> = self
            .app_state
            .read(cx)
            .connections()
            .keys()
            .copied()
            .collect();
        let stale_metric_ids: Vec<Uuid> = self
            .instance_metrics_cache
            .keys()
            .filter(|id| !connected_profile_ids.contains(id))
            .copied()
            .collect();
        for profile_id in stale_metric_ids {
            self.clear_instance_catalog_cache(profile_id);
        }

        let stale_root_section_ids: Vec<Uuid> = self
            .root_section_cache
            .profile_ids()
            .chain(
                self.pending_root_section_fetches
                    .keys()
                    .map(|key| &key.profile_id),
            )
            .filter(|id| !connected_profile_ids.contains(id))
            .copied()
            .collect();
        for profile_id in stale_root_section_ids {
            self.clear_root_section_cache(profile_id, cx);
        }

        // A connection with root sections that finishes while its profile
        // node is already expanded (the active profile expands by default)
        // never re-fires the expand handler, so its sections would sit on the
        // "Loading..." placeholder until a manual collapse/expand. Kick the
        // fetch here instead; `spawn_fetch_root_sections` deduplicates
        // against the cache and in-flight fetches.
        let expanded_root_section_ids: Vec<Uuid> = connected_profile_ids
            .iter()
            .filter(|id| {
                self.profile_has_root_sections(**id, cx) && self.profile_node_is_expanded(**id, cx)
            })
            .copied()
            .collect();
        for profile_id in expanded_root_section_ids {
            self.spawn_fetch_root_sections(profile_id, cx);
        }

        self.cleanup_stale_overrides(cx);

        let items = self.build_tree_items_with_overrides(cx);
        self.fetch_visible_inline_lists(&items, cx);
        self.prune_connection_selection(&items);
        self.visible_entry_count = Self::count_visible_entries(&items);
        self.gutter_metadata = compute_gutter_map(&items);

        if let Some(ref menu) = self.context_menu
            && Self::find_item_index_in_tree(&items, &menu.item_id, &mut 0).is_none()
        {
            self.context_menu = None;
        }

        let selected_index = selected_id
            .as_deref()
            .and_then(|id| Self::find_item_index_in_tree(&items, id, &mut 0));
        self.syncing_expansion = true;
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);

            state.set_selected_index(selected_index, cx);
        });
        self.syncing_expansion = false;
        cx.notify();
    }

    /// Remove cached schema data for a specific database, causing the next
    /// expansion to re-fetch from the driver.
    pub(super) fn invalidate_database_cache(
        &mut self,
        profile_id: Uuid,
        db_name: &str,
        cx: &mut Context<Self>,
    ) {
        self.table_details_retry
            .borrow_mut()
            .retain(|_, (key, _, _)| {
                key.profile_id() != profile_id || key.node_key().database() != Some(db_name)
            });
        self.table_details_recovery.retain(|_, (key, _)| {
            key.profile_id() != profile_id || key.node_key().database() != Some(db_name)
        });
        let state = self.app_state.read(cx);
        if let (Some(connection), Some(generation)) = (
            state
                .connections()
                .get(&profile_id)
                .map(|connected| connected.connection.clone()),
            state.profile_session_generation(profile_id),
        ) {
            self.known_invalidated_databases.borrow_mut().insert(
                (profile_id, db_name.to_string()),
                (std::sync::Arc::downgrade(&connection), generation),
            );
        }
        self.app_state.update(cx, |state, _cx| {
            state.invalidate_database_schema(profile_id, db_name);
            if let Some(conn) = state.connections_mut().get_mut(&profile_id) {
                conn.table_details.retain(|(db, _, _), _| db != db_name);
                conn.schema_types.retain(|key, _| key.database != db_name);
                conn.schema_indexes.retain(|key, _| key.database != db_name);
                conn.schema_foreign_keys
                    .retain(|key, _| key.database != db_name);

                if let Some(schema) = conn.schema.as_mut() {
                    schema.remove_database(db_name);
                }
            }

            // Slot removal goes through the manager seam so the per-target
            // slot revision advances; the dropped entry is discarded, exactly
            // like the previous raw map removal, without touching the active
            // database.
            if let Some(db_conn) = state.take_database_connection(profile_id, db_name) {
                drop(db_conn);
            }
        });
    }

    /// Remove the cached column/index details for a single table or collection,
    /// so the next expansion re-fetches from the driver.
    pub(super) fn invalidate_object_cache(
        &mut self,
        profile_id: Uuid,
        cache_db: &str,
        target: &SchemaDropTarget,
        cx: &mut Context<Self>,
    ) {
        self.app_state.update(cx, |state, _cx| {
            if let Some(conn) = state.connections_mut().get_mut(&profile_id) {
                // Drop every schema slot for the object name: a drop target
                // may or may not carry a schema, and stale details under any
                // schema must not survive the invalidation.
                conn.table_details
                    .retain(|(db, _, table), _| db != cache_db || table != &target.name);

                if target.kind == SchemaObjectKind::Table {
                    let target_schema = target.schema.as_deref();

                    conn.schema_indexes.retain(|key, indexes| {
                        if key.database != cache_db || key.schema.as_deref() != target_schema {
                            return true;
                        }

                        indexes.retain(|index| index.table_name != target.name);
                        !indexes.is_empty()
                    });

                    conn.schema_foreign_keys.retain(|key, foreign_keys| {
                        if key.database != cache_db || key.schema.as_deref() != target_schema {
                            return true;
                        }

                        foreign_keys.retain(|foreign_key| foreign_key.table_name != target.name);
                        !foreign_keys.is_empty()
                    });
                }

                if let Some(db_schema) = conn.database_schemas.get_mut(cache_db) {
                    db_schema.remove_object(target);
                }

                if let Some(db_conn) = conn.database_connections.get_mut(cache_db)
                    && let Some(schema) = db_conn.schema.as_mut()
                {
                    schema.remove_object(target);
                }

                if let Some(schema) = conn.schema.as_mut() {
                    schema.remove_object(target);
                }
            }
        });
    }

    fn cleanup_stale_overrides(&mut self, cx: &Context<Self>) {
        let state = self.app_state.read(cx);

        self.expansion_overrides.retain(|item_id, _expanded| {
            if self.loading_items.contains(item_id) {
                return true;
            }

            match parse_node_id(item_id) {
                Some(SchemaNodeId::TypesFolder {
                    profile_id,
                    database,
                    schema,
                }) => !state.needs_schema_types(profile_id, &database, Some(&schema)),
                Some(SchemaNodeId::SchemaIndexesFolder {
                    profile_id,
                    database,
                    schema,
                }) => !state.needs_schema_indexes(profile_id, &database, Some(&schema)),
                Some(SchemaNodeId::SchemaForeignKeysFolder {
                    profile_id,
                    database,
                    schema,
                }) => !state.needs_schema_foreign_keys(profile_id, &database, Some(&schema)),
                Some(SchemaNodeId::RoutinesFolder {
                    profile_id,
                    database,
                    schema,
                }) => !state.needs_schema_routines(profile_id, &database, Some(&schema)),
                _ => true,
            }
        });
    }
}

#[cfg(test)]
mod keyboard_expand_tests {
    use crate::Sidebar;
    use crate::table_loading::object_tree_adapter_tests::{
        AdapterFakeConnection, connect_profile, register_per_database_driver, snapshot_naming,
        test_app_state,
    };
    use datatab_core::SchemaNodeId;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use std::sync::atomic::Ordering;
    use uuid::Uuid;

    fn profile_row_expanded(
        sidebar: &Entity<Sidebar>,
        profile_id: Uuid,
        cx: &mut VisualTestContext,
    ) -> Option<bool> {
        let item_id = SchemaNodeId::Profile { profile_id }.to_string().into();

        sidebar.update(cx, |sidebar, cx| {
            let tree = sidebar.tree_state.read(cx);
            tree.index_of(&item_id)
                .and_then(|index| tree.entry(index))
                .map(|entry| entry.is_expanded())
        })
    }

    /// `l` on a disconnected connection connects it through the same path as
    /// Enter and the row's chevron, and the row opens once connected.
    #[gpui::test]
    async fn expanding_a_disconnected_connection_connects_it(cx: &mut TestAppContext) {
        let state = test_app_state(cx);
        let (connect_calls, _) = register_per_database_driver(&state, cx);

        let mut profile = datatab_core::ConnectionProfile::new(
            "expand-test",
            datatab_core::DbConfig::default_postgres(),
        );
        let profile_id = Uuid::new_v4();
        profile.id = profile_id;
        state.update(cx, |state, _| {
            state.profiles_mut().push(profile);
            state.connection_tree_mut().add_node(
                datatab_core::ConnectionTreeNode::new_connection_ref(profile_id, None, 1000),
            );
        });

        let (sidebar, cx) =
            cx.add_window_view(|window, cx| Sidebar::new(state.clone(), window, cx));

        sidebar.update(cx, |sidebar, cx| {
            sidebar.select_first(cx);
            sidebar.expand(cx);
        });
        cx.run_until_parked();

        assert_eq!(connect_calls.load(Ordering::SeqCst), 1);
        state.read_with(cx, |state, _| {
            assert!(state.connections().contains_key(&profile_id));
        });
        assert_eq!(profile_row_expanded(&sidebar, profile_id, cx), Some(true));
    }

    /// `h` on an open, connected connection collapses the row and leaves the
    /// connection up.
    #[gpui::test]
    async fn collapsing_a_connected_connection_keeps_it_connected(cx: &mut TestAppContext) {
        let state = test_app_state(cx);
        let profile_id = Uuid::new_v4();
        let main = datatab_core::DatabaseInfo {
            name: "main".into(),
            is_current: true,
        };

        let connection = AdapterFakeConnection::lazy();
        *connection.databases.lock().expect("fake databases") = vec![main.clone()];
        connect_profile(
            &state,
            cx,
            profile_id,
            connection,
            Some(snapshot_naming(vec![main])),
        );

        let (sidebar, cx) =
            cx.add_window_view(|window, cx| Sidebar::new(state.clone(), window, cx));

        sidebar.update(cx, |sidebar, cx| sidebar.select_first(cx));
        assert_eq!(profile_row_expanded(&sidebar, profile_id, cx), Some(true));

        sidebar.update(cx, |sidebar, cx| sidebar.collapse(cx));
        cx.run_until_parked();

        assert_eq!(profile_row_expanded(&sidebar, profile_id, cx), Some(false));
        state.read_with(cx, |state, _| {
            assert!(state.connections().contains_key(&profile_id));
        });
    }
}
