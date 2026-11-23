use super::*;
use crate::ui::labels::settings_default_connection_name;
use datatab_core::LogErr as _;

impl Workspace {
    /// Makes every Settings window of this app state forward its events to
    /// this workspace, whichever surface opens it.
    pub(in crate::ui::views::workspace) fn register_settings_forwarder(
        &self,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.entity().downgrade();
        datatab_ui_windows::settings::set_settings_event_forwarder(
            &self.app_state,
            std::rc::Rc::new(move |event, cx| {
                workspace
                    .update(cx, |this, cx| this.handle_settings_event(event, cx))
                    .log_err();
            }),
            cx,
        );
    }

    fn handle_settings_event(
        &mut self,
        event: &datatab_ui_windows::settings::SettingsEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            datatab_ui_windows::settings::SettingsEvent::OpenScript { path } => {
                self.open_script_from_path(path.clone(), cx);
            }
            datatab_ui_windows::settings::SettingsEvent::OpenLoginModal {
                provider_name,
                profile_name,
                url,
                login,
            } => {
                self.pending_login_modal_open = Some(PendingSettingsLogin {
                    provider_name: provider_name.clone(),
                    profile_name: profile_name.clone(),
                    url: url.clone(),
                    login: login.clone(),
                });
                cx.notify();
            }
        }
    }

    pub(in crate::ui::views::workspace) fn open_settings(&self, cx: &mut Context<Self>) {
        datatab_ui_windows::settings::open_or_focus_settings(
            self.app_state.clone(),
            None,
            cx,
            |_settings, _cx| {},
        );
    }

    /// Shows a login the Settings window started in the login modal, and
    /// ties the two together: the modal closes when the Settings form stops
    /// waiting for the login, and the modal's Cancel button cancels the login
    /// as the form's own Cancel does.
    pub(in crate::ui::views::workspace) fn show_settings_login(
        &mut self,
        pending: PendingSettingsLogin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PendingSettingsLogin {
            provider_name,
            profile_name,
            url,
            login,
        } = pending;

        // The form may have stopped waiting since it sent the URL.
        if !login.is_waiting(cx) {
            return;
        }

        self.last_settings_login_id = self.last_settings_login_id.wrapping_add(1);
        let login_id = self.last_settings_login_id;

        let Some(ended) = login.on_ended(cx, move |this, cx| {
            this.settings_login_ended(login_id, cx);
        }) else {
            return;
        };

        self.settings_login = Some(ShownSettingsLogin {
            id: login_id,
            login,
            _ended: ended,
        });
        self.login_modal.update(cx, |modal, cx| {
            modal.open_for_login(login_id, provider_name, profile_name, url, window, cx);
        });
    }

    fn settings_login_ended(&mut self, login_id: u64, cx: &mut Context<Self>) {
        self.settings_login.take_if(|shown| shown.id == login_id);
        self.login_modal.update(cx, |modal, cx| {
            modal.login_ended(login_id, cx);
        });
    }

    /// The user pressed Cancel in the login modal while it showed the
    /// Settings login `login_id`.
    pub(in crate::ui::views::workspace) fn cancel_settings_login(
        &mut self,
        login_id: u64,
        cx: &mut Context<Self>,
    ) {
        if let Some(shown) = self.settings_login.take_if(|shown| shown.id == login_id) {
            shown.login.cancel(cx);
        }
    }

    pub(in crate::ui::views::workspace) fn open_login_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile_name = self
            .app_state
            .read(cx)
            .active_connection()
            .map(|connected| connected.profile.name.clone())
            .unwrap_or_else(settings_default_connection_name);

        // "Auth Provider" is the persisted default display name for a manually
        // opened login (no specific provider is known yet); it is stored, not
        // just displayed, so it stays in English like other persisted identities.
        self.login_modal.update(cx, |modal, cx| {
            modal.open_manual("Auth Provider", profile_name, None, window, cx);
        });
    }

    pub(in crate::ui::views::workspace) fn open_auth_profiles_settings(
        &self,
        cx: &mut Context<Self>,
    ) {
        datatab_ui_windows::settings::open_or_focus_settings(
            self.app_state.clone(),
            Some(datatab_ui_windows::settings::SettingsSectionId::AuthProfiles),
            cx,
            |_settings, _cx| {},
        );
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent glob with `#[gpui::test]` sends the
    // gpui_macros expansion into unbounded recursion.
    use crate::ui::views::workspace::Workspace;
    use datatab_core::LogErr as _;
    use datatab_ui_base::AppStateEntity;
    use datatab_ui_windows::settings::{SettingsEvent, SettingsSectionId};
    use gpui::{AppContext as _, TestAppContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Removes the file when the test ends, also when an assert fails.
    struct RemoveOnDrop(std::path::PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).log_err();
        }
    }

    /// A Settings window opened by a surface that passes no setup of its own
    /// (Auth Profiles shortcut, connection manager, About) still reaches the
    /// workspace: its events are forwarded once.
    #[gpui::test]
    fn settings_opened_without_setup_still_reaches_the_workspace(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        cx.update(datatab_ui_base::keymap::init_keymap);
        let app_state = cx.update(|cx| {
            cx.new(|_| {
                let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory()
                    .expect("in-memory storage");
                AppStateEntity::new_with_storage_runtime(runtime).expect("test storage setup")
            })
        });

        let holder: Rc<RefCell<Option<gpui::Entity<Workspace>>>> = Rc::default();
        let (_, window) = cx.add_window_view({
            let app_state = app_state.clone();
            let holder = holder.clone();
            move |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(app_state, window, cx));
                holder.replace(Some(workspace.clone()));
                gpui_component::Root::new(workspace, window, cx)
            }
        });
        window.run_until_parked();
        let workspace = holder.borrow().clone().expect("workspace created");

        let path = std::env::temp_dir().join(format!(
            "datatab-settings-forwarder-{}.lua",
            std::process::id()
        ));
        std::fs::write(&path, "-- forwarded").expect("write the script");
        let _script_file = RemoveOnDrop(path.clone());

        let captured = Rc::new(RefCell::new(None));
        let captured_for_setup = captured.clone();
        window.update(|_, cx| {
            datatab_ui_windows::settings::open_or_focus_settings(
                app_state.clone(),
                Some(SettingsSectionId::AuthProfiles),
                cx,
                move |settings, _| {
                    captured_for_setup.replace(Some(settings.clone()));
                },
            );
        });
        let settings = captured.borrow().clone().expect("the window opened");
        window.update(|_, cx| {
            settings.update(cx, |_, cx| {
                cx.emit(SettingsEvent::OpenScript { path: path.clone() });
            });
        });
        window.run_until_parked();

        let opened = window.update(|_, cx| workspace.read(cx).tab_manager.read(cx).len());
        assert_eq!(opened, 1, "the workspace opened the script once");
    }
}
