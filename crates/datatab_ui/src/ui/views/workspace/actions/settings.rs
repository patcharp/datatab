use super::*;
use crate::ui::labels::settings_default_connection_name;

impl Workspace {
    pub(in crate::ui::views::workspace) fn open_settings(&self, cx: &mut Context<Self>) {
        let workspace = cx.entity().clone();
        datatab_ui_windows::settings::open_or_focus_settings(
            self.app_state.clone(),
            None,
            cx,
            move |settings, cx| {
                cx.subscribe(
                    settings,
                    move |_settings, event: &datatab_ui_windows::settings::SettingsEvent, cx| {
                        workspace.update(cx, |this, cx| match event {
                            datatab_ui_windows::settings::SettingsEvent::OpenScript { path } => {
                                this.open_script_from_path(path.clone(), cx);
                            }
                            datatab_ui_windows::settings::SettingsEvent::OpenLoginModal {
                                provider_name,
                                profile_name,
                                url,
                                login,
                            } => {
                                this.pending_login_modal_open = Some(PendingSettingsLogin {
                                    provider_name: provider_name.clone(),
                                    profile_name: profile_name.clone(),
                                    url: url.clone(),
                                    login: login.clone(),
                                });
                                cx.notify();
                            }
                        });
                    },
                )
                .detach();
            },
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
