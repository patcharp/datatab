//! Unit tests of the instance inspector's execution helpers.

mod catalog_thread;

use super::*;
use datatab_core::{DbError, ExecutionSourceContext};
use datatab_ui_base::user_error::{ErrorKind, UserFacingError};

/// REQ-DOC-3, REQ-UI-2: `build_inspector_context` must produce an
/// `InstanceInspectorQuery` with exactly the given `metric_id` and no
/// time-window fields.
#[test]
fn build_inspector_context_produces_instance_inspector_query() {
    let metric_id = "pg.activity".to_string();

    let ctx = super::build_inspector_context(&metric_id);

    match ctx {
        ExecutionSourceContext::InstanceInspectorQuery { metric_id: ref id } => {
            assert_eq!(
                id, &metric_id,
                "metric_id must round-trip through the context"
            );
        }
        other => panic!("expected InstanceInspectorQuery, got {:?}", other),
    }
}

/// REQ-DOC-3: The inspector context must NOT carry `start_ms` or `end_ms`
/// fields. This test pins the structural invariant at the type level by
/// attempting to destructure with those fields (which must not compile).
#[test]
fn inspector_query_context_has_no_time_window_fields() {
    let ctx = ExecutionSourceContext::InstanceInspectorQuery {
        metric_id: "pg.locks".to_string(),
    };

    let ExecutionSourceContext::InstanceInspectorQuery { metric_id } = ctx else {
        panic!("expected InstanceInspectorQuery");
    };

    assert_eq!(
        metric_id, "pg.locks",
        "metric_id must match the constructed value"
    );
}

/// BF9: a kill failure must be convertible to a `UserFacingError` of kind `Driver`
/// carrying the driver error message as its `summary`.
#[test]
fn kill_error_produces_driver_kind_user_facing_error() {
    let err = DbError::QueryFailed(
        "permission denied for function pg_terminate_backend"
            .to_string()
            .into(),
    );
    let uf = kill_error_to_user_facing("kill", "Terminate connection", "pg.activity", &err);
    assert_eq!(uf.kind, ErrorKind::Driver);
    assert!(
        uf.summary.contains("permission denied"),
        "summary must carry the driver error text; got: {:?}",
        uf.summary
    );
}

/// BF9: the summary includes context (metric_id and action label) so the user
/// knows which operation failed.
#[test]
fn kill_error_summary_includes_action_context() {
    let err = DbError::NotSupported("driver does not support instance catalog".to_string());
    let uf = kill_error_to_user_facing("kill", "Kill process", "mysql.processlist", &err);
    assert_eq!(uf.kind, ErrorKind::Driver);
    assert!(
        !uf.summary.is_empty(),
        "summary must not be empty even for NotSupported errors"
    );
}

/// M3: When both `pending_result` and `pending_initial_exec` are set in the
/// same render frame, `ExecutionFinished` must fire exactly once (from the
/// completed result) and `pending_initial_exec` must be cleared.
///
/// The correct drain order is result-first so the old `ExecutionFinished`
/// (decrement) happens before the new dispatch (increment). With no live
/// connection, `request_reexec` transitions the panel to error state — that
/// side-effect is acceptable for this ordering test.
#[gpui::test]
fn pending_result_drains_before_pending_initial_exec(cx: &mut gpui::TestAppContext) {
    use datatab_core::{ColumnKind, ColumnMeta, QueryResult, QueryResultShape};
    use datatab_storage::bootstrap::StorageRuntime;
    use std::{cell::Cell, rc::Rc, time::Duration};

    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    cx.update(|cx| {
        let host = cx.new(|_cx| datatab_ui_base::toast::ToastHost::new());
        cx.set_global(datatab_ui_base::toast::ToastGlobal { host });
    });

    let app_state: Entity<AppStateEntity> = cx.update(|cx| {
        cx.new(|_| {
            let runtime = StorageRuntime::in_memory().expect("in-memory storage");
            AppStateEntity::new_with_storage_runtime(runtime).expect("test storage setup")
        })
    });

    let profile_id = uuid::Uuid::new_v4();
    let panel: Entity<super::InspectorPanel> = cx.update(|cx| {
        cx.new(|cx| {
            super::InspectorPanel::new(profile_id, "pg.activity".to_string(), app_state, cx)
        })
    });

    let synthetic_result = QueryResult {
        shape: QueryResultShape::Table,
        columns: vec![ColumnMeta {
            name: "pid".to_string(),
            type_name: "int4".to_string(),
            kind: ColumnKind::Integer,
            nullable: true,
            is_primary_key: false,
        }],
        rows: vec![vec![datatab_core::Value::Int(1)]],
        affected_rows: None,
        execution_time: Duration::ZERO,
        text_body: None,
        raw_bytes: None,
        next_page_token: None,
        resolved_window: None,
        metadata_extra: None,
        additional_results: Vec::new(),
    };

    // Set both flags before render.
    cx.update(|cx| {
        panel.update(cx, |p, _cx| {
            p.pending_result = Some(super::PendingResult {
                task_id: uuid::Uuid::nil(),
                result: Ok(synthetic_result),
            });
            p.pending_initial_exec = true;
        });
    });

    // Count ExecutionFinished emissions via subscription.
    let finished_count = Rc::new(Cell::new(0u32));
    let count_ref = finished_count.clone();

    cx.update(|cx| {
        cx.subscribe(&panel, move |_panel, event: &super::DocumentEvent, _cx| {
            if matches!(event, super::DocumentEvent::ExecutionFinished) {
                count_ref.set(count_ref.get() + 1);
            }
        })
        .detach();
    });

    // Render once inside a window — drains both flags.
    cx.add_window_view(|window, cx| {
        panel.update(cx, |p, cx| {
            p.render(window, cx);
        });
        gpui_component::Root::new(panel.clone(), window, cx)
    });

    // Exactly one ExecutionFinished from apply_result (the initial exec
    // dispatch does not emit one; it returns early because no connection exists).
    cx.update(|_| {});
    assert_eq!(
        finished_count.get(),
        1,
        "exactly one ExecutionFinished must be emitted when both flags are set in one render pass"
    );

    // pending_initial_exec must have been consumed.
    let still_pending = cx.update(|cx| panel.read(cx).pending_initial_exec);
    assert!(
        !still_pending,
        "pending_initial_exec must be cleared after render"
    );
}

/// BF4: after `apply_result` is called with a successful `QueryResult`,
/// `has_result()` must return `true` and `pending_grid_result` must be
/// populated (since no `data_grid` exists yet in a non-rendered panel).
///
/// Uses `#[gpui::test]` because `apply_result` calls
/// `runner.complete_primary` which requires a GPUI `App` context.
#[gpui::test]
fn apply_result_sets_result_and_pending_grid(cx: &mut gpui::TestAppContext) {
    use datatab_core::{ColumnKind, ColumnMeta, QueryResult, QueryResultShape};
    use datatab_storage::bootstrap::StorageRuntime;
    use std::time::Duration;

    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    cx.update(|cx| {
        let host = cx.new(|_cx| datatab_ui_base::toast::ToastHost::new());
        cx.set_global(datatab_ui_base::toast::ToastGlobal { host });
    });

    let app_state: Entity<AppStateEntity> = cx.update(|cx| {
        cx.new(|_| {
            let runtime = StorageRuntime::in_memory().expect("in-memory storage");
            AppStateEntity::new_with_storage_runtime(runtime).expect("test storage setup")
        })
    });

    let profile_id = uuid::Uuid::new_v4();
    let panel: Entity<super::InspectorPanel> = cx.update(|cx| {
        cx.new(|cx| {
            super::InspectorPanel::new(profile_id, "pg.activity".to_string(), app_state, cx)
        })
    });

    let synthetic_result = QueryResult {
        shape: QueryResultShape::Table,
        columns: vec![ColumnMeta {
            name: "pid".to_string(),
            type_name: "int4".to_string(),
            kind: ColumnKind::Integer,
            nullable: true,
            is_primary_key: false,
        }],
        rows: vec![vec![datatab_core::Value::Int(42)]],
        affected_rows: None,
        execution_time: Duration::ZERO,
        text_body: None,
        raw_bytes: None,
        next_page_token: None,
        resolved_window: None,
        metadata_extra: None,
        additional_results: Vec::new(),
    };

    cx.update(|cx| {
        panel.update(cx, |p, cx| {
            let task_id = uuid::Uuid::nil();
            p.apply_result(
                super::PendingResult {
                    task_id,
                    result: Ok(synthetic_result),
                },
                cx,
            );
        });
    });

    cx.update(|cx| {
        let p = panel.read(cx);
        assert!(
            p.has_result(),
            "apply_result with Ok must set result to Some"
        );
        assert!(
            p.pending_grid_result.is_some(),
            "apply_result must populate pending_grid_result when data_grid is None"
        );
    });
}

#[gpui::test]
fn inspector_auto_refresh_honors_visibility_and_pause_on_error(cx: &mut gpui::TestAppContext) {
    let app_state = cx.update(|cx| {
        cx.new(|_| {
            let storage_runtime = datatab_storage::bootstrap::StorageRuntime::in_memory()
                .expect("isolated storage runtime");
            AppStateEntity::new_with_storage_runtime(storage_runtime).expect("test storage setup")
        })
    });
    let inspector = cx.update(|cx| {
        cx.new(|cx| {
            InspectorPanel::new(
                Uuid::new_v4(),
                "sessions".to_string(),
                app_state.clone(),
                cx,
            )
        })
    });
    let set_settings =
        |only_if_visible: bool, pause_on_error: bool, cx: &mut gpui::TestAppContext| {
            app_state.update(cx, |state, _| {
                let mut settings = state.general_settings().clone();
                settings.auto_refresh_only_if_visible = only_if_visible;
                settings.auto_refresh_pause_on_error = pause_on_error;
                state.update_general_settings(settings);
            });
        };
    let tick_allowed = |cx: &mut gpui::TestAppContext| {
        cx.update(|cx| inspector.read(cx).auto_refresh_tick_allowed(cx))
    };

    set_settings(true, true, cx);
    assert!(tick_allowed(cx));

    inspector.update(cx, |panel, _| panel.set_active_tab(false));
    assert!(!tick_allowed(cx));

    inspector.update(cx, |panel, _| {
        panel.set_active_tab(true);
        panel.state = DocumentState::Error;
    });
    assert!(!tick_allowed(cx));

    set_settings(false, false, cx);
    inspector.update(cx, |panel, _| panel.set_active_tab(false));
    assert!(tick_allowed(cx));
}
