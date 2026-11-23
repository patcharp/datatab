use super::new_workspace_with;
use crate::ui::views::workspace::Workspace;
use datatab_core::{ConnectionProfile, DbConfig, SchemaNodeId};
use datatab_storage::bootstrap::StorageRuntime;
use datatab_storage::repositories::connection_profiles::ConnectionProfileRepository;
use datatab_ui_base::AppStateEntity;
use datatab_ui_base::app_state_entity::AppStateChanged;
use datatab_ui_base::keyboard_coverage::FrameCapture;
use datatab_ui_base::toast::{ToastGlobal, ToastHost};
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};
use gpui_component::Root;
use uuid::Uuid;

struct Fixture {
    workspace: Entity<Workspace>,
    app_state: Entity<AppStateEntity>,
    profile_ids: [Uuid; 2],
    repository: ConnectionProfileRepository,
}

fn setup(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
    let runtime = StorageRuntime::in_memory().expect("test storage");
    let repository = runtime.connection_profiles();
    let app_state =
        cx.new(|_| AppStateEntity::new_with_storage_runtime(runtime).expect("test app state"));
    cx.update(|cx| {
        let host = cx.new(|_| ToastHost::new());
        cx.set_global(ToastGlobal { host });
    });
    let (workspace, app_state, window) = new_workspace_with(cx, app_state);
    let profile_ids = window.update(|_, cx| {
        app_state.update(cx, |state, cx| {
            *state.secret_store().write().expect("secret store") =
                Box::new(datatab_core::NoopSecretStore);
            let ids = ["Alpha", "Beta"].map(|name| {
                let mut profile = ConnectionProfile::new(
                    name,
                    DbConfig::SQLite {
                        path: ":memory:".into(),
                        connection_id: None,
                    },
                );
                profile.save_password = false;
                let id = profile.id;
                state.add_profile_in_folder(profile, None);
                id
            });
            cx.emit(AppStateChanged);
            ids
        })
    });
    window.run_until_parked();
    (
        Fixture {
            workspace,
            app_state,
            profile_ids,
            repository,
        },
        window,
    )
}

fn open_editor(
    fixture: &Fixture,
    profile_id: Uuid,
    window: &mut VisualTestContext,
) -> WindowHandle<Root> {
    window.update(|_, cx| {
        fixture.workspace.update(cx, |workspace, cx| {
            workspace.open_connection_manager_for_edit(profile_id, cx);
        });
        *fixture
            .app_state
            .read(cx)
            .connection_edit_windows
            .get(&profile_id)
            .expect("registered editor")
    })
}

fn close_editor(editor: WindowHandle<Root>, window: &mut VisualTestContext) {
    editor
        .update(window, |_, window, _| window.remove_window())
        .expect("close editor");
    window.run_until_parked();
}

fn assert_profiles_exist(fixture: &Fixture, window: &mut VisualTestContext) {
    window.update(|_, cx| assert_eq!(fixture.app_state.read(cx).profiles().len(), 2));
    for profile_id in fixture.profile_ids {
        assert!(
            fixture
                .repository
                .get(&profile_id.to_string())
                .expect("read stored profile")
                .is_some()
        );
    }
}

fn assert_refused(fixture: &Fixture, editor: WindowHandle<Root>, window: &mut VisualTestContext) {
    window.run_until_parked();
    assert_profiles_exist(fixture, window);
    window.update(|_, cx| {
        let workspace = fixture.workspace.read(cx);
        assert!(
            !workspace.sidebar.read(cx).has_delete_modal(),
            "no pending dialog"
        );
        assert!(
            !workspace.sidebar.read(cx).has_pending_delete(),
            "no pending keyboard delete"
        );
        assert!(
            !workspace.modal_delete_connection.read(cx).is_visible(),
            "no delete dialog"
        );
        assert_eq!(
            cx.active_window(),
            Some(editor.into()),
            "the editor is activated"
        );
    });
    let mut editor = VisualTestContext::from_window(editor.into(), window);
    let capture = FrameCapture::observe(&mut editor);
    let frame = capture.frame(&mut editor);
    assert_eq!(
        frame
            .nodes()
            .find(|(_, node)| node.id() == "connection-edit-delete-notice")
            .and_then(|(_, node)| frame.accessibility_node(node))
            .and_then(|node| node.label().map(str::to_owned)),
        Some(
            "This connection is open for editing. Close this window before deleting it."
                .to_string()
        ),
        "the refusal is visible inside the edit window",
    );
}

#[gpui::test]
fn connection_edit_second_open_focuses_the_existing_window(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    window.update(|window, _| window.activate_window());
    let second = open_editor(&fixture, fixture.profile_ids[0], window);
    window.update(|_, cx| {
        assert_eq!(cx.windows().len(), 2, "second open must reuse the editor");
        assert_eq!(editor, second);
        assert_eq!(
            cx.active_window(),
            Some(editor.into()),
            "the editor is activated"
        );
    });
}

#[gpui::test]
fn connection_edit_close_removes_handle_and_reopens(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    close_editor(editor, window);
    window.update(|_, cx| {
        assert!(
            fixture
                .app_state
                .read(cx)
                .connection_edit_windows
                .is_empty(),
            "closing removes the handle"
        );
    });
    let reopened = open_editor(&fixture, fixture.profile_ids[0], window);
    assert_ne!(editor.window_id(), reopened.window_id());
    window.update(|_, cx| assert_eq!(cx.windows().len(), 2));

    let other = open_editor(&fixture, fixture.profile_ids[1], window);
    window.update(|_, cx| assert_eq!(cx.windows().len(), 3));
    close_editor(reopened, window);
    window.update(|_, cx| {
        let editors = &fixture.app_state.read(cx).connection_edit_windows;
        assert_eq!(editors.len(), 1, "another profile keeps its editor");
        assert_eq!(editors.get(&fixture.profile_ids[1]), Some(&other));
    });
}

#[gpui::test]
fn connection_edit_stale_handle_is_replaced(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    close_editor(editor, window);
    window.update(|_, cx| {
        fixture.app_state.update(cx, |state, _| {
            state
                .connection_edit_windows
                .insert(fixture.profile_ids[0], editor);
        });
    });
    let reopened = open_editor(&fixture, fixture.profile_ids[0], window);
    assert_ne!(editor.window_id(), reopened.window_id());
    window.update(|_, cx| assert_eq!(cx.windows().len(), 2));
}

#[gpui::test]
fn connection_edit_successful_save_removes_handle(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    let mut editor_window = VisualTestContext::from_window(editor.into(), window);
    editor_window.run_until_parked();
    #[cfg(target_os = "macos")]
    editor_window.simulate_keystrokes("cmd-s");
    #[cfg(not(target_os = "macos"))]
    editor_window.simulate_keystrokes("ctrl-s");
    editor_window.run_until_parked();
    assert!(
        editor.root(window).is_err(),
        "successful save closes the editor"
    );
    window.update(|_, cx| {
        assert!(
            fixture
                .app_state
                .read(cx)
                .connection_edit_windows
                .is_empty(),
            "saving removes the handle"
        )
    });
    assert_profiles_exist(&fixture, window);
}

#[gpui::test]
fn connection_edit_single_delete_refuses_then_works_after_close(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    let item_id = SchemaNodeId::Profile {
        profile_id: fixture.profile_ids[0],
    }
    .to_string();
    window.update(|window, cx| {
        window.activate_window();
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.show_delete_confirm_modal(&item_id, cx)
            });
    });
    assert_refused(&fixture, editor, window);
    close_editor(editor, window);
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.show_delete_confirm_modal(&item_id, cx)
            });
    });
    window.run_until_parked();
    window.update(|_, cx| {
        let modal = fixture.workspace.read(cx).modal_delete_connection.clone();
        assert!(modal.read(cx).is_visible());
        modal.update(cx, |modal, cx| modal.confirm(cx));
    });
    window.run_until_parked();
    assert!(
        fixture
            .repository
            .get(&fixture.profile_ids[0].to_string())
            .expect("read profile")
            .is_none()
    );
}

#[gpui::test]
fn connection_edit_keyboard_delete_refuses(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    window.update(|window, cx| {
        window.activate_window();
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.select_first(cx);
                sidebar.request_delete_selected(cx);
            });
    });
    assert_refused(&fixture, editor, window);
    close_editor(editor, window);
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.request_delete_selected(cx);
                assert!(sidebar.has_pending_delete());
            });
    });
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.request_delete_selected(cx);
            });
    });
    assert_refused(&fixture, editor, window);
}

#[gpui::test]
fn connection_edit_pending_keyboard_delete_rechecks_the_whole_selection(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[1], window);
    window.update(|window, cx| {
        window.activate_window();
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.select_first(cx);
                sidebar.request_delete_selected(cx);
                assert!(sidebar.has_pending_delete());
                sidebar.extend_select_next(cx);
                assert!(sidebar.has_multi_selection());
                sidebar.request_delete_selected(cx);
            });
    });
    assert_refused(&fixture, editor, window);
}

fn select_both(fixture: &Fixture, window: &mut VisualTestContext) {
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.select_first(cx);
                sidebar.toggle_current_selection(cx);
                sidebar.select_next(cx);
                sidebar.toggle_current_selection(cx);
            });
    });
}

#[gpui::test]
fn connection_edit_multi_delete_refuses_the_whole_selection(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let editor = open_editor(&fixture, fixture.profile_ids[1], window);
    select_both(&fixture, window);
    window.update(|window, cx| {
        window.activate_window();
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| sidebar.request_delete_selected(cx));
    });
    assert_refused(&fixture, editor, window);
    close_editor(editor, window);
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| sidebar.request_delete_selected(cx));
    });
    window.run_until_parked();
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                assert!(sidebar.has_delete_modal());
                sidebar.confirm_modal_delete(cx);
            });
    });
    window.run_until_parked();
    for profile_id in fixture.profile_ids {
        assert!(
            fixture
                .repository
                .get(&profile_id.to_string())
                .expect("read profile")
                .is_none()
        );
    }
}

#[gpui::test]
fn connection_edit_single_confirm_rechecks_open_editors(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    let item_id = SchemaNodeId::Profile {
        profile_id: fixture.profile_ids[0],
    }
    .to_string();
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                sidebar.show_delete_confirm_modal(&item_id, cx)
            });
    });
    window.run_until_parked();
    let editor = open_editor(&fixture, fixture.profile_ids[0], window);
    window.update(|_, cx| {
        let modal = fixture.workspace.read(cx).modal_delete_connection.clone();
        assert!(modal.read(cx).is_visible());
        modal.update(cx, |modal, cx| modal.confirm(cx));
    });
    assert_refused(&fixture, editor, window);
}

#[gpui::test]
fn connection_edit_multi_confirm_rechecks_before_deleting_anything(cx: &mut TestAppContext) {
    let (fixture, window) = setup(cx);
    select_both(&fixture, window);
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| sidebar.request_delete_selected(cx));
    });
    window.run_until_parked();
    let editor = open_editor(&fixture, fixture.profile_ids[1], window);
    window.update(|_, cx| {
        fixture
            .workspace
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| {
                assert!(sidebar.has_delete_modal());
                sidebar.confirm_modal_delete(cx);
            });
    });
    assert_refused(&fixture, editor, window);
}
