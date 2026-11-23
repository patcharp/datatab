use super::super::ConnectionManagerWindow;
use datatab_components::accessibility::{FrameCapture, accessible_name};
use datatab_core::{AppStyle, ConnectionProfile, DbConfig, ThemeSetting};
use datatab_storage::bootstrap::StorageRuntime;
use datatab_ui_base::AppStateEntity;
use datatab_ui_base::app_state_entity::ConnectionEditAttentionRequested;
use datatab_ui_base::toast::{ToastGlobal, ToastHost};
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use gpui_component::Root;

#[gpui::test]
fn connection_edit_attention_shows_a_named_notice_only_in_its_editor(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    cx.update(datatab_ui_base::keymap::init_keymap);
    cx.update(|cx| {
        let host = cx.new(|_| ToastHost::new());
        cx.set_global(ToastGlobal { host });
    });
    let app_state: Entity<AppStateEntity> = cx.new(|_| {
        AppStateEntity::new_with_storage_runtime(StorageRuntime::in_memory().expect("storage"))
            .expect("app state")
    });
    let profile = ConnectionProfile::new("Editing", DbConfig::default_sqlite());
    app_state.update(cx, |state, _| {
        *state.secret_store().write().expect("secret store") =
            Box::new(datatab_core::NoopSecretStore);
        state.add_profile_in_folder(profile.clone(), None);
    });
    let (_, window) = cx.add_window_view(|window, cx| {
        let manager = cx.new(|cx| {
            ConnectionManagerWindow::new_for_edit(app_state.clone(), &profile, window, cx)
        });
        Root::new(manager, window, cx)
    });
    let capture = FrameCapture::observe(window);
    app_state.update(window, |_, cx| {
        cx.emit(ConnectionEditAttentionRequested(uuid::Uuid::new_v4()));
    });
    assert_eq!(
        notice(&capture, window),
        None,
        "another profile stays quiet"
    );
    app_state.update(window, |_, cx| {
        cx.emit(ConnectionEditAttentionRequested(profile.id))
    });
    for setting in [ThemeSetting::Dark, ThemeSetting::Light] {
        window.update(|window, cx| {
            datatab_components::theme::apply_theme(setting, AppStyle::default(), Some(window), cx);
        });
        assert_eq!(
            notice(&capture, window),
            Some(
                "This connection is open for editing. Close this window before deleting it."
                    .to_string()
            ),
        );
    }
}

fn notice(capture: &FrameCapture, window: &mut VisualTestContext) -> Option<String> {
    accessible_name(&capture.frame(window), "connection-edit-delete-notice")
}
