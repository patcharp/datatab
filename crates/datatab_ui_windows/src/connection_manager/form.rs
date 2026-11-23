use crate::ssh_shared;
use datatab_components::components::form_renderer;
use datatab_core::secrecy::SecretString;
use datatab_core::values::ValueRef;
use datatab_core::{
    AccessKind, CancelToken, ConnectionMcpGovernance, ConnectionOverrides, ConnectionProfile,
    DbConfig, FormFieldKind, HookPhase, SshTunnelConfig,
};
use datatab_ui_base::hook_phase_runner::{DetachedHookScope, HookPhaseState, run_hook_phase};
use datatab_ui_base::toast::{Toast, now_hms};
use datatab_ui_base::user_error::{ErrorKind, UserFacingError, report_error};
use gpui::*;
use log::info;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::mcp_bindings;
use super::{ConnectionManagerWindow, DeferredAction, DismissEvent, StoredPassword, TestStatus};

const PRIMARY_PASSWORD_SAVE_ERROR: &str =
    "Unable to save the connection password to the system keyring. Unlock it and try again.";

/// Whether the form may delete a keyring entry it no longer wants. The form
/// has not seen the entry while the read is pending or after the keyring
/// refused it, so the user cannot have meant to drop it, and a keyring
/// unlocked since would otherwise lose a secret nobody chose to remove.
fn may_delete_unseen(stored: StoredPassword, what: &str, profile: &ConnectionProfile) -> bool {
    match stored {
        StoredPassword::Known => true,
        StoredPassword::NotInKeyring => {
            log::debug!("Keeping the saved {what}: it does not live in the keyring");
            false
        }
        StoredPassword::Loading | StoredPassword::Unreadable | StoredPassword::Skipped => {
            log::warn!(
                "Keeping the saved {what} for profile {}: the keyring had not been read when \
                 the form was saved, or could not be",
                profile.id
            );
            false
        }
    }
}

struct ProfileSecretChanges {
    password: Option<SecretString>,
    delete_password: bool,
    ssh_secret: Option<SecretString>,
    delete_ssh_secret: bool,
}

struct SaveFailure {
    messages: Vec<String>,
    report: bool,
    secret_fields_failed: bool,
}

impl SaveFailure {
    fn primary() -> Self {
        Self {
            messages: vec![PRIMARY_PASSWORD_SAVE_ERROR.to_string()],
            report: true,
            secret_fields_failed: false,
        }
    }

    fn with_rollback(
        mut self,
        rollback: Result<(), datatab_app::SecretFieldsMayHaveChanged>,
    ) -> Self {
        if let Err(changed) = rollback {
            let message = datatab_i18n::t!(
                "form.validation.secret_fields_may_have_changed",
                fields = changed.quoted_labels()
            );
            if self.secret_fields_failed {
                self.messages.push(message);
            } else {
                self.messages.insert(0, message);
            }
        }
        self
    }
}

/// Only plain data and the shared store cross onto the background executor.
#[allow(clippy::result_large_err)]
fn write_profile_secrets(
    store: &std::sync::RwLock<Box<dyn datatab_core::SecretStore>>,
    profile: &ConnectionProfile,
    fields: datatab_app::app_state::SecretFieldWrite,
    changes: ProfileSecretChanges,
) -> Result<datatab_app::SecretFieldChanges, SaveFailure> {
    let fields = fields.apply().map_err(|error| {
        let failure = SaveFailure {
            messages: vec![datatab_i18n::t!(
                "form.validation.secret_field_keyring_failed",
                field = error.label
            )],
            report: false,
            secret_fields_failed: true,
        };
        match error.may_have_changed {
            Some(changed) => failure.with_rollback(Err(changed)),
            None => failure,
        }
    })?;
    let secrets = datatab_core::secret_manager::ProfileSecrets::new(store);
    if let Some(password) = changes.password {
        if secrets.save_password(profile, &password).is_err() {
            return Err(SaveFailure::primary().with_rollback(fields.restore(store)));
        }
    } else if changes.delete_password {
        secrets.delete_password(profile);
    }
    if let Some(secret) = changes.ssh_secret {
        secrets.save_ssh_password(profile, &secret);
    } else if changes.delete_ssh_secret {
        secrets.delete_ssh_password(profile);
    }
    Ok(fields)
}

enum ProfileSaveCommit {
    Saved,
    Deleted,
}

#[allow(clippy::result_large_err)]
fn commit_saved_profile(
    app_state: &Entity<datatab_ui_base::AppStateEntity>,
    profile: ConnectionProfile,
    guarded_password: &str,
    is_edit: bool,
    target_folder_id: Option<uuid::Uuid>,
    cx: &mut App,
) -> Result<ProfileSaveCommit, datatab_core::DbError> {
    let saved_profile_id = profile.id;
    app_state.update(cx, |state, cx| {
        if is_edit {
            if !state
                .profiles()
                .iter()
                .any(|item| item.id == saved_profile_id)
            {
                return Ok(ProfileSaveCommit::Deleted);
            }
            state.update_profile_from_connection_manager(profile, guarded_password)?;

            // If the edited profile is currently connected, surface a
            // reconnect prompt — the sidebar consumes this flag on the
            // next AppStateChanged and shows a toast with the choice.
            // The profile change itself is already persisted; only the
            // live session needs the explicit reconnect to pick it up.
            if state.connections().contains_key(&saved_profile_id) {
                state.pending_edit_reconnect_prompt = Some(saved_profile_id);
            }
        } else {
            state.add_profile_in_folder(profile, target_folder_id);
        }

        save_profile_governance(state, saved_profile_id, cx);
        cx.emit(datatab_ui_base::AppStateChanged);
        Ok(ProfileSaveCommit::Saved)
    })
}

fn delete_saved_profile_secrets(
    store: &std::sync::RwLock<Box<dyn datatab_core::SecretStore>>,
    profile: &ConnectionProfile,
) -> SaveFailure {
    let store = match store.read() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    let secret_refs = profile
        .keyring_secret_fields
        .iter()
        .map(|field| datatab_core::connection_field_secret_ref(&profile.id, field))
        .chain([profile.secret_ref(), profile.ssh_secret_ref()]);
    let mut cleanup_failed = false;
    for secret_ref in secret_refs {
        if store.delete(&secret_ref).is_err() {
            cleanup_failed = true;
        }
    }
    let mut messages = vec![datatab_i18n::t!(
        "form.validation.connection_deleted_while_saving"
    )];
    if cleanup_failed {
        log::error!(
            "Failed to remove secrets for deleted profile {}",
            profile.id
        );
        messages.push(datatab_i18n::t!(
            "form.validation.deleted_connection_secret_cleanup_failed"
        ));
    }
    SaveFailure {
        messages,
        report: false,
        secret_fields_failed: false,
    }
}

fn save_profile_governance(
    #[allow(unused_variables)] state: &mut datatab_ui_base::AppStateEntity,
    #[allow(unused_variables)] saved_profile_id: uuid::Uuid,
    #[allow(unused_variables)] cx: &mut Context<datatab_ui_base::AppStateEntity>,
) {
    #[cfg(feature = "mcp")]
    {
        if let Some(governance) = state
            .profiles()
            .iter()
            .find(|item| item.id == saved_profile_id)
            .and_then(|item| item.mcp_governance.clone())
        {
            let assignments = governance
                .policy_bindings
                .into_iter()
                .map(|binding| datatab_policy::ConnectionPolicyAssignment {
                    actor_id: binding.actor_id,
                    scope: datatab_policy::PolicyBindingScope {
                        connection_id: saved_profile_id.to_string(),
                    },
                    role_ids: binding.role_ids,
                    policy_ids: binding.policy_ids,
                })
                .collect();

            if let Err(e) = state.save_mcp_connection_policy_assignment(
                datatab_mcp::ConnectionPolicyAssignmentDto {
                    connection_id: saved_profile_id.to_string(),
                    assignments,
                },
            ) {
                report_error(
                    UserFacingError::new(
                        ErrorKind::Config,
                        datatab_i18n::t!(
                            "connection_manager.mcp_governance_error.save_policy",
                            error = e
                        ),
                    ),
                    cx,
                );
            }
        } else if let Err(e) = state.save_mcp_connection_policy_assignment(
            datatab_mcp::ConnectionPolicyAssignmentDto {
                connection_id: saved_profile_id.to_string(),
                assignments: Vec::new(),
            },
        ) {
            report_error(
                UserFacingError::new(
                    ErrorKind::Config,
                    datatab_i18n::t!(
                        "connection_manager.mcp_governance_error.clear_policy",
                        error = e
                    ),
                ),
                cx,
            );
        }

        cx.emit(datatab_ui_base::McpRuntimeEventRaised {
            event: datatab_mcp::McpRuntimeEvent::ConnectionPolicyUpdated {
                connection_id: saved_profile_id.to_string(),
            },
        });
    }
}

struct PreparedProfileSave {
    profile: ConnectionProfile,
    guarded_password: String,
    is_edit: bool,
    target_folder_id: Option<uuid::Uuid>,
}

#[allow(clippy::result_large_err)]
async fn persist_profile_after_secrets(
    result: Result<datatab_app::SecretFieldChanges, SaveFailure>,
    save: PreparedProfileSave,
    app_state: Entity<datatab_ui_base::AppStateEntity>,
    store: Arc<std::sync::RwLock<Box<dyn datatab_core::SecretStore>>>,
    executor: BackgroundExecutor,
    cx: &mut AsyncApp,
) -> Option<SaveFailure> {
    match result {
        Err(failure) => Some(failure),
        Ok(fields) => {
            let saved_profile = save.profile.clone();
            let result = cx.update(|cx| {
                commit_saved_profile(
                    &app_state,
                    save.profile,
                    &save.guarded_password,
                    save.is_edit,
                    save.target_folder_id,
                    cx,
                )
            });
            match result {
                Ok(ProfileSaveCommit::Saved) => None,
                Ok(ProfileSaveCommit::Deleted) => {
                    // Restoring old values would recreate secrets the profile deletion removed.
                    Some(
                        executor
                            .spawn(
                                async move { delete_saved_profile_secrets(&store, &saved_profile) },
                            )
                            .await,
                    )
                }
                Err(error) => {
                    let failure = if matches!(error, datatab_core::DbError::InvalidProfile(_)) {
                        SaveFailure {
                            messages: vec![datatab_i18n::t!(
                                "form.validation.broken_password_unresolved"
                            )],
                            report: false,
                            secret_fields_failed: false,
                        }
                    } else {
                        SaveFailure::primary()
                    };
                    let rollback = executor.spawn(async move { fields.restore(&store) }).await;
                    Some(failure.with_rollback(rollback))
                }
            }
        }
    }
}

/// The SSL mode id and the certificate paths the shared Transport control holds.
///
/// Drivers hardcode a default and their form declares no TLS field, so the
/// selection is written into the built config here: one source of truth for the
/// control and the profile.
fn apply_ssl_selection(
    config: &mut DbConfig,
    selected_mode: Option<&str>,
    root_cert: Option<String>,
    client_cert: Option<String>,
    client_key: Option<String>,
) {
    if let Some(selected) = selected_mode {
        match config {
            DbConfig::Postgres { ssl_mode, .. }
            | DbConfig::MySQL { ssl_mode, .. }
            | DbConfig::MongoDB { ssl_mode, .. }
            | DbConfig::Redis { ssl_mode, .. }
            | DbConfig::Redshift { ssl_mode, .. }
            | DbConfig::ClickHouse { ssl_mode, .. }
            | DbConfig::Cassandra { ssl_mode, .. } => {
                *ssl_mode = Some(selected.to_string());
            }
            DbConfig::SqlServer {
                ssl_mode,
                trust_server_certificate,
                ..
            } => {
                *ssl_mode = Some(selected.to_string());
                // `collect_form_values` does not carry the selected mode, so
                // the driver's `build_config` fell back to its "on" default
                // and set `trust_server_certificate: true`. Derive trust from
                // the user's selection: the form clears it for `required`.
                // The driver also enforces legacy `verify-*` modes even
                // when this saved trust flag is true.
                *trust_server_certificate = !matches!(selected, "required");
            }
            _ => {}
        }
    }

    match config {
        DbConfig::Postgres {
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        }
        | DbConfig::MySQL {
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        }
        | DbConfig::MongoDB {
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        }
        | DbConfig::Redis {
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        }
        | DbConfig::Redshift {
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        }
        | DbConfig::ClickHouse {
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        }
        | DbConfig::Cassandra {
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        } => {
            *ssl_root_cert_path = root_cert;
            *ssl_client_cert_path = client_cert;
            *ssl_client_key_path = client_key;
        }
        DbConfig::SqlServer {
            ssl_root_cert_path, ..
        } => {
            *ssl_root_cert_path = root_cert;
        }
        _ => {}
    }
}

/// A text input's value as typed, or `None` when the user left it empty.
///
/// The value is not trimmed: a stored certificate path is kept exactly as it
/// was entered, which is how the field behaved before this was a helper.
fn text_input_value(input: &Entity<gpui_component::input::InputState>, cx: &App) -> Option<String> {
    let value = input.read(cx).value().to_string();
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

impl ConnectionManagerWindow {
    fn collect_mcp_governance(&self, cx: &Context<Self>) -> Option<ConnectionMcpGovernance> {
        if !self.mcp_tab.conn_mcp_enabled {
            return None;
        }

        let mut policy_bindings = self.mcp_tab.bindings.clone();

        // Defensive: the currently selected client's widgets should already be in
        // sync via `handle_mcp_binding_field_change`, but flush once more before
        // returning `policy_bindings` so a save can never race a still-pending
        // widget event.
        if let Some(actor_id) = self.mcp_tab.selected_actor_id.clone() {
            let (role_ids, policy_ids) = self.read_selected_mcp_role_and_policy_ids(cx);
            mcp_bindings::apply_selection(&mut policy_bindings, &actor_id, role_ids, policy_ids);
        }

        Some(ConnectionMcpGovernance {
            enabled: true,
            policy_bindings,
        })
    }

    pub(super) fn validate_form(&mut self, require_name: bool, cx: &mut Context<Self>) -> bool {
        self.validation_errors.clear();

        if require_name {
            let name = self.form.input_name.read(cx).value().to_string();
            if name.trim().is_empty() {
                self.validation_errors
                    .push(datatab_i18n::t!("form.validation.connection_name_required"));
            }
        }

        let Some(driver) = &self.form.selected_driver else {
            self.validation_errors
                .push(datatab_i18n::t!("form.validation.no_driver_selected"));
            return false;
        };

        if driver.metadata().ssl_modes.is_some_and(|modes| {
            !modes
                .iter()
                .any(|mode| mode.id == self.form.selected_ssl_mode)
        }) {
            self.validation_errors.push(datatab_i18n::t!(
                "form.validation.field_required",
                field = datatab_i18n::t!("connection_manager.field.ssl_mode")
            ));
        }

        let form = driver.form_definition();

        for tab in form.tabs.iter().filter(|t| t.id != "ssh") {
            for section in &tab.sections {
                for field in &section.fields {
                    if field.id == "password" || field.kind == FormFieldKind::Checkbox {
                        continue;
                    }

                    let field_enabled = self.is_field_enabled(field);
                    if !field_enabled {
                        continue;
                    }

                    let value = self
                        .form
                        .driver_inputs
                        .get(&field.id)
                        .map(|input| input.read(cx).value().to_string())
                        .unwrap_or_default();

                    if field.required
                        && value.trim().is_empty()
                        && !self.has_dynamic_value_ref_for_field(&field.id, cx)
                        && !self.saved_keyring_secret_fields(cx).contains(&field.id)
                        && !self.kept_stored_value_refs(cx).contains_key(&field.id)
                    {
                        self.validation_errors.push(datatab_i18n::t!(
                            "form.validation.field_required",
                            field = field.label.clone()
                        ));
                    }

                    if !value.trim().is_empty()
                        && field.kind == FormFieldKind::Number
                        && value.parse::<u16>().is_err()
                    {
                        self.validation_errors.push(datatab_i18n::t!(
                            "form.validation.field_invalid_number",
                            field = field.label.clone()
                        ));
                    }
                }
            }
        }

        if self.access.ssh_enabled && form.supports_ssh() {
            // While an SSH config alias is active the empty host and user
            // are correct: the alias is the only source of the target (A7).
            let ssh_alias_active = self.ssh_config_alias(cx).is_some();
            let ssh_host = self.access.input_ssh_host.read(cx).value().to_string();
            if !ssh_alias_active && ssh_host.trim().is_empty() {
                self.validation_errors
                    .push(datatab_i18n::t!("form.validation.ssh_host_required"));
            }

            let ssh_user = self.access.input_ssh_user.read(cx).value().to_string();
            if !ssh_alias_active && ssh_user.trim().is_empty() {
                self.validation_errors
                    .push(datatab_i18n::t!("form.validation.ssh_user_required"));
            }

            let ssh_port_str = self.access.input_ssh_port.read(cx).value().to_string();
            if !ssh_port_str.trim().is_empty() && ssh_port_str.parse::<u16>().is_err() {
                self.validation_errors
                    .push(datatab_i18n::t!("form.validation.ssh_port_invalid"));
            }
        }

        if let Some(auth_profile_id) = self.auth_profile.selected_auth_profile_id
            && !self
                .app_state
                .read(cx)
                .list_auth_profiles()
                .iter()
                .any(|profile| profile.id == auth_profile_id)
        {
            self.validation_errors.push(datatab_i18n::t!(
                "form.validation.auth_profile_not_found",
                id = auth_profile_id.to_string()
            ));
        }

        let uses_dynamic_auth_sources = self
            .collect_value_refs(cx)
            .values()
            .any(|value_ref| matches!(value_ref, ValueRef::Secret { .. } | ValueRef::Auth { .. }));

        if uses_dynamic_auth_sources {
            let Some(auth_profile_id) = self.auth_profile.selected_auth_profile_id else {
                self.validation_errors.push(datatab_i18n::t!(
                    "form.validation.dynamic_auth_profile_required"
                ));
                return self.validation_errors.is_empty();
            };

            let bound_profile = self
                .app_state
                .read(cx)
                .list_auth_profiles()
                .into_iter()
                .find(|profile| profile.id == auth_profile_id);

            if let Some(profile) = bound_profile {
                if self
                    .app_state
                    .read(cx)
                    .auth_provider_by_id(&profile.provider_id)
                    .is_none()
                {
                    self.validation_errors.push(datatab_i18n::t!(
                        "form.validation.auth_profile_no_provider",
                        name = profile.name
                    ));
                }
            } else {
                // Already reported above in the missing-profile guard; no
                // duplicate message needed.
            }
        }

        self.validate_hook_bindings(cx);

        // Drivers reject combinations the per-field checks cannot see (for
        // example mutually exclusive credential sources); surface their
        // message instead of failing the later config build silently.
        if self.validation_errors.is_empty()
            && let Some(driver) = self.form.selected_driver.clone()
        {
            let values = self.collect_form_values(driver.form_definition(), cx);
            if let Err(error) = driver.build_config(&values) {
                self.validation_errors.push(error.to_string());
            }
        }

        self.validation_errors.is_empty()
    }

    pub(super) fn build_ssh_config(&self, cx: &Context<Self>) -> Option<SshTunnelConfig> {
        if !self.access.ssh_enabled {
            return None;
        }

        let host = self.access.input_ssh_host.read(cx).value().to_string();
        let port_str = self.access.input_ssh_port.read(cx).value().to_string();
        let user = self.access.input_ssh_user.read(cx).value().to_string();
        let key_path_str = self.access.input_ssh_key_path.read(cx).value().to_string();
        let ssh_config_host = self.ssh_config_alias(cx);

        Some(ssh_shared::build_ssh_config_with_alias(
            &host,
            &port_str,
            &user,
            self.access.ssh_auth_method,
            &key_path_str,
            ssh_config_host.as_deref(),
        ))
    }

    pub(super) fn build_config(&self, cx: &Context<Self>) -> Option<DbConfig> {
        let driver = self.form.selected_driver.as_ref()?;
        let values = self.collect_form_values(driver.form_definition(), cx);

        let mut config = match driver.build_config(&values) {
            Ok(config) => config,
            Err(e) => {
                log::error!("Failed to build config: {}", e);
                return None;
            }
        };

        // Persist the shared Transport control's selection, which every driver
        // with an `ssl_mode` field shares.
        let selected_mode = if self.form.selected_ssl_mode.is_empty() {
            None
        } else {
            Some(self.form.selected_ssl_mode.clone())
        };
        let (root_visible, client_visible) = driver
            .metadata()
            .ssl_cert_fields
            .map(|fields| fields.visible_for_mode(&self.form.selected_ssl_mode))
            .unwrap_or((false, false));
        let ssl_root_cert = root_visible
            .then(|| text_input_value(&self.form.ssl_ca_cert_input, cx))
            .flatten();
        let ssl_client_cert = client_visible
            .then(|| text_input_value(&self.form.ssl_client_cert_input, cx))
            .flatten();
        let ssl_client_key = client_visible
            .then(|| text_input_value(&self.form.ssl_client_key_input, cx))
            .flatten();
        apply_ssl_selection(
            &mut config,
            selected_mode.as_deref(),
            ssl_root_cert,
            ssl_client_cert,
            ssl_client_key,
        );

        let ssh_tunnel_profile_id = self.access.selected_ssh_tunnel_id;
        let ssh_tunnel = if ssh_tunnel_profile_id.is_some() {
            None
        } else {
            self.build_ssh_config(cx)
        };

        config.assign_ssh_tunnel(ssh_tunnel, ssh_tunnel_profile_id);

        Some(config)
    }

    fn with_stored_profile<R>(
        &self,
        cx: &App,
        read: impl FnOnce(&ConnectionProfile) -> R,
    ) -> Option<R> {
        let profile_id = self.editing_profile_id?;
        self.app_state
            .read(cx)
            .profiles()
            .iter()
            .find(|profile| profile.id == profile_id)
            .map(read)
    }

    /// The secret fields whose value the keyring holds for the profile being
    /// edited. An empty input for one of them keeps that value.
    fn saved_keyring_secret_fields(&self, cx: &App) -> std::collections::BTreeSet<String> {
        self.with_stored_profile(cx, |profile| profile.keyring_secret_fields.clone())
            .unwrap_or_default()
    }

    /// The form has a value-source selector for host, database, user and
    /// password only. A stored ref of any other field (for example an import's
    /// `Env` source for an RPC driver's `api_token`) has no control to show or
    /// change it, so an edit keeps it as stored. A field that has a control
    /// and whose value the user changed since the form loaded is left out: the
    /// typed value wins.
    fn kept_stored_value_refs(
        &self,
        cx: &Context<Self>,
    ) -> std::collections::HashMap<String, ValueRef> {
        let current = self
            .form
            .selected_driver
            .as_ref()
            .map(|driver| self.collect_form_values(driver.form_definition(), cx))
            .unwrap_or_default();
        let mut kept = self
            .with_stored_profile(cx, |stored| stored.value_refs.clone())
            .unwrap_or_default();
        kept.retain(|field, _| {
            let loaded = self
                .loaded_form_values
                .get(field)
                .map_or("", String::as_str);
            let typed = current.get(field).is_some_and(|value| value != loaded);
            !Self::VALUE_SOURCE_FIELDS.contains(&field.as_str()) && !typed
        });
        kept
    }

    pub(super) fn build_profile(&self, cx: &Context<Self>) -> Option<ConnectionProfile> {
        let name = self.form.input_name.read(cx).value().to_string();
        let kind = self.selected_kind()?;
        let driver_id = self.selected_driver_id()?;
        let config = self.build_config(cx)?;

        let mut profile = if let Some(existing_id) = self.editing_profile_id {
            let mut p = ConnectionProfile::new_with_driver(name, kind, driver_id, config);
            p.id = existing_id;
            p
        } else {
            ConnectionProfile::new_with_driver(name, kind, driver_id, config)
        };

        profile.save_password = self.form.form_save_password;
        profile.keyring_secret_fields = self.saved_keyring_secret_fields(cx);
        profile.environment = self.form.environment;
        profile.navigator_view = self.form.navigator_view;
        profile.show_all_databases = self.form.show_all_databases;
        profile.read_only_flag = self.form.read_only;
        profile.proxy_profile_id = self.access.selected_proxy_id;
        profile.auth_profile_id = self.auth_profile.selected_auth_profile_id;
        profile.value_refs = self.collect_value_refs(cx);
        for (field, value_ref) in self.kept_stored_value_refs(cx) {
            profile.value_refs.entry(field).or_insert(value_ref);
        }
        profile.settings_overrides = self.collect_connection_overrides(cx);
        profile.connection_settings = self.collect_connection_settings(cx);
        profile.hook_bindings = self.collect_hook_bindings(cx);
        profile.mcp_governance = self.collect_mcp_governance(cx);

        // Collect access kind — keep SSH/proxy profile selections as references instead
        // of flattening them into inline connection fields.
        let access_kind = if let Some(ssh_tunnel_profile_id) = self.access.selected_ssh_tunnel_id {
            Some(AccessKind::Ssh {
                ssh_tunnel_profile_id,
            })
        } else if let Some(proxy_profile_id) = self.access.selected_proxy_id {
            Some(AccessKind::Proxy { proxy_profile_id })
        } else {
            self.access.access_kind.clone()
        };
        profile.access_kind = access_kind;

        if profile.hook_bindings.is_some() {
            profile.hooks = None;
        } else if let Some(existing_id) = self.editing_profile_id {
            let existing_hooks = self
                .app_state
                .read(cx)
                .profiles()
                .iter()
                .find(|item| item.id == existing_id)
                .and_then(|item| item.hooks.clone());
            profile.hooks = existing_hooks;
        }

        Some(profile)
    }

    pub(super) fn get_ssh_secret(&self, cx: &Context<Self>) -> Option<String> {
        if !self.access.ssh_enabled {
            return None;
        }

        let passphrase = self
            .access
            .input_ssh_key_passphrase
            .read(cx)
            .value()
            .to_string();
        let password = self.access.input_ssh_password.read(cx).value().to_string();

        ssh_shared::get_ssh_secret(self.access.ssh_auth_method, &passphrase, &password)
    }

    pub(super) fn save_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.defer_until_secrets_read(DeferredAction::Save) {
            return;
        }

        self.apply_pending_auth_profile(window, cx);

        if !self.validate_form(true, cx) {
            cx.notify();
            return;
        }

        let Some(profile) = self.build_profile(cx) else {
            return;
        };

        // Refused before any secret is written: a profile whose driver
        // declares a reserved kind could not be loaded again.
        if datatab_app::AppState::check_profile_can_be_saved(&profile).is_err() {
            self.validation_errors
                .push(datatab_i18n::t!("form.validation.reserved_database_kind"));
            cx.notify();
            return;
        }

        let password = self.form.input_password.read(cx).value().to_string();
        let password_source_is_literal = self
            .form
            .password_value_source_selector
            .read(cx)
            .is_literal(cx);
        let guarded_password = if password_source_is_literal {
            password.clone()
        } else {
            String::new()
        };
        if let Err(error) = self
            .app_state
            .read(cx)
            .check_connection_manager_save(&profile, &guarded_password)
        {
            let message = match error {
                datatab_app::app_state::ProfileSaveError::UnknownFormLiteral { .. } => {
                    let message = datatab_i18n::t!("form.validation.unknown_form_literal_ref");
                    report_error(UserFacingError::new(ErrorKind::Config, message.clone()), cx);
                    message
                }
                datatab_app::app_state::ProfileSaveError::Storage(_) => {
                    let message = datatab_i18n::t!("form.validation.profile_storage_read_failed");
                    report_error(
                        UserFacingError::new(ErrorKind::Storage, message.clone()),
                        cx,
                    );
                    message
                }
                _ => datatab_i18n::t!("form.validation.broken_password_unresolved"),
            };
            self.validation_errors.push(message);
            cx.notify();
            return;
        }

        self.start_profile_save(
            profile,
            (password, password_source_is_literal),
            guarded_password,
            window,
            cx,
        );
    }

    fn start_profile_save(
        &mut self,
        mut profile: ConnectionProfile,
        password_input: (String, bool),
        guarded_password: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        info!("Saving profile: {}", profile.id);
        let (mut password, password_source_is_literal) = password_input;
        let fields = self
            .app_state
            .read(cx)
            .prepare_external_secret_fields(&mut profile);
        if let Some(uri_password) = profile.config.strip_uri_password()
            && password.is_empty()
        {
            password = uri_password;
        }
        let changes = ProfileSecretChanges {
            password: (password_source_is_literal && profile.save_password && !password.is_empty())
                .then(|| SecretString::from(password)),
            delete_password: (!password_source_is_literal || !profile.save_password)
                && may_delete_unseen(self.form.stored_password, "password", &profile),
            ssh_secret: self
                .form
                .form_save_ssh_secret
                .then(|| self.get_ssh_secret(cx))
                .flatten()
                .map(SecretString::from),
            delete_ssh_secret: !self.form.form_save_ssh_secret
                && may_delete_unseen(self.form.stored_ssh_secret, "SSH secret", &profile),
        };
        for warning in self.save_warnings(&profile) {
            Toast::warning(warning).meta_right(now_hms()).push(cx);
        }
        self.saving = true;
        cx.notify();
        let store = self.app_state.read(cx).secret_store();
        let write = cx.background_executor().spawn({
            let store = store.clone();
            let profile = profile.clone();
            async move { write_profile_secrets(&store, &profile, fields, changes) }
        });
        let app_state = self.app_state.clone();
        let save = PreparedProfileSave {
            profile,
            guarded_password,
            is_edit: self.editing_profile_id.is_some(),
            target_folder_id: self.target_folder_id,
        };
        let window_handle = window.window_handle();
        let executor = cx.background_executor().clone();
        // The transaction outlives the window. Every failure is consumed here,
        // after rollback, even if there is no form left to display it in.
        cx.spawn(async move |this, cx| {
            let result = write.await;
            let failure =
                persist_profile_after_secrets(result, save, app_state, store, executor, cx).await;
            cx.update(|cx| {
                let applied = window_handle
                    .update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.finish_profile_save(failure.as_ref(), window, cx)
                        })
                        .is_ok()
                    })
                    .unwrap_or(false);
                if !applied && let Some(failure) = failure {
                    report_error(
                        UserFacingError::new(ErrorKind::Storage, failure.messages.join(" ")),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn finish_profile_save(
        &mut self,
        failure: Option<&SaveFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saving = false;
        if let Some(failure) = failure {
            self.validation_errors.extend(
                failure
                    .messages
                    .iter()
                    .filter(|message| message.as_str() != PRIMARY_PASSWORD_SAVE_ERROR)
                    .cloned(),
            );
            if failure.report {
                report_error(
                    UserFacingError::new(ErrorKind::Storage, PRIMARY_PASSWORD_SAVE_ERROR),
                    cx,
                );
            }
            cx.notify();
        } else {
            cx.emit(DismissEvent);
            window.remove_window();
        }
    }

    fn save_warnings(&self, profile: &ConnectionProfile) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.settings_tab.conn_override_refresh_interval
            && profile
                .settings_overrides
                .as_ref()
                .is_none_or(|ov| ov.refresh_interval_secs.is_none())
        {
            warnings.push(datatab_i18n::t!("form.warning.refresh_interval_invalid"));
        }

        if let Some(ref conn_settings) = profile.connection_settings
            && let Some(driver) = &self.form.selected_driver
            && let Some(schema) = driver.settings_schema()
        {
            warnings.extend(form_renderer::validate_values(&schema, conn_settings));
        }
        warnings
    }

    pub(super) fn test_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(error) = self.value_ref_notice.clone() {
            self.test_status = TestStatus::Failed;
            self.test_error = Some(error.to_string());
            cx.notify();
            return;
        }
        if self.defer_until_secrets_read(DeferredAction::TestConnection) {
            return;
        }

        self.apply_pending_auth_profile(window, cx);

        if !self.validate_form(false, cx) {
            cx.notify();
            return;
        }

        self.test_status = TestStatus::Testing;
        self.test_error = None;
        self.test_result = None;
        cx.notify();

        let Some(profile) = self.build_profile(cx) else {
            self.test_status = TestStatus::Failed;
            self.test_error = Some(datatab_i18n::t!("form.error.build_profile_failed"));
            cx.notify();
            return;
        };

        let Some(driver) = self.form.selected_driver.clone() else {
            self.test_status = TestStatus::Failed;
            self.test_error = Some(datatab_i18n::t!("form.validation.no_driver_selected"));
            cx.notify();
            return;
        };

        if let Err(error) = driver.validate_profile(&profile) {
            self.test_status = TestStatus::Failed;
            self.test_error = Some(error.to_string());
            cx.notify();
            return;
        }

        let profile_name = profile.name.clone();
        let app_state = self.app_state.clone();
        let hook_context = self.app_state.read(cx).build_hook_context(&profile);
        let hooks = self.app_state.read(cx).resolve_profile_hooks(&profile);
        let hook_cancel_token = CancelToken::new();
        let detached_hook_scope = DetachedHookScope::default();
        let cleanup_cancel_token = hook_cancel_token.clone();
        let cleanup_detached_hook_scope = detached_hook_scope.clone();
        let cleanup_app_state = app_state.clone();
        let this = cx.entity().clone();

        let pipeline_input = if profile.uses_pipeline() {
            match self
                .app_state
                .read(cx)
                .build_pipeline_input_for_profile(profile.clone(), hook_cancel_token.clone())
            {
                Ok(input) => Some(input),
                Err(error) => {
                    self.test_status = TestStatus::Failed;
                    self.test_error = Some(error);
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };

        let password = self.form.input_password.read(cx).value().to_string();
        let password = (!password.is_empty()).then(|| SecretString::from(password));
        let ssh_secret = self.get_ssh_secret(cx).map(SecretString::from);
        let secret_store = self.app_state.read(cx).secret_store();

        cx.spawn(async move |_this, cx| {
            let profile_id = profile.id;
            let profile_name_for_hooks = profile_name.clone();
            let profile_name_for_cleanup = profile_name.clone();
            let phase_cx = cx.clone();
            let cleanup_cx = cx.clone();
            let background_executor = cx.background_executor().clone();

            let result = run_test_connection_orchestration(
                hooks,
                move |phase, phase_hooks, context| {
                    let app_state = app_state.clone();
                    let profile_name = profile_name_for_hooks.clone();
                    let hook_cancel_token = hook_cancel_token.clone();
                    let detached_hook_scope = detached_hook_scope.clone();
                    let mut phase_cx = phase_cx.clone();

                    Box::pin(async move {
                        run_hook_phase(
                            app_state,
                            profile_id,
                            profile_name,
                            phase,
                            phase_hooks,
                            context,
                            Some(hook_cancel_token),
                            &detached_hook_scope,
                            &mut phase_cx,
                        )
                        .await
                    })
                },
                move |drop_guards| {
                    let driver = driver.clone();
                    let profile = profile.clone();
                    let password = password.clone();
                    let ssh_secret = ssh_secret.clone();
                    let secret_store = secret_store.clone();
                    let background_executor = background_executor.clone();

                    Box::pin(async move {
                        if let Some(pipeline_input) = pipeline_input {
                            background_executor
                                .spawn(async move {
                                    let (state_tx, _state_rx) =
                                        datatab_core::pipeline_state_channel();
                                    let pipeline_output =
                                        datatab_core::run_pipeline(pipeline_input, &state_tx)
                                            .await
                                            .map_err(|error| {
                                                datatab_i18n::t!(
                                                    "form.error.pipeline_stage_failed",
                                                    stage = error.stage,
                                                    source = error.source.to_string()
                                                )
                                            })?;

                                    let mut profile = pipeline_output.resolved_profile;
                                    if pipeline_output.access_handle.is_tunneled() {
                                        profile.config.redirect_to_tunnel(
                                            pipeline_output.access_handle.local_port(),
                                        );
                                    }
                                    if let Some(filled) =
                                        datatab_core::profile_with_keyring_fields_from(
                                            &profile,
                                            driver.form_definition(),
                                            &secret_store,
                                        )?
                                    {
                                        profile = filled;
                                    }

                                    // The pipeline only yields a password when the
                                    // profile carries a `ValueRef` for it. Fall back to
                                    // the form password (prefilled from the keyring when
                                    // editing) so a pipeline profile is not probed
                                    // without credentials.
                                    let overrides = ConnectionOverrides::new(
                                        pipeline_output.resolved_password.or(password),
                                    );
                                    let access_handle_drop = TestConnectionProbeResource {
                                        name: "pipeline access handle",
                                        drop_guard: drop_guards.access_handle,
                                    };
                                    let connection = driver
                                        .connect_with_overrides(&profile, &overrides)
                                        .map_err(|error| error.to_string())?;
                                    let connection_drop = TestConnectionProbeResource {
                                        name: "probe connection",
                                        drop_guard: drop_guards.connection,
                                    };

                                    drop(connection);
                                    drop(connection_drop);
                                    drop(pipeline_output.access_handle);
                                    drop(access_handle_drop);

                                    Ok(datatab_core::TestConnectionResult::default())
                                })
                                .await
                        } else {
                            background_executor
                                .spawn(async move {
                                    let filled = datatab_core::profile_with_keyring_fields_from(
                                        &profile,
                                        driver.form_definition(),
                                        &secret_store,
                                    )?;
                                    let start = std::time::Instant::now();
                                    driver
                                        .test_connection_rich_with_secrets(
                                            filled.as_ref().unwrap_or(&profile),
                                            password.as_ref(),
                                            ssh_secret.as_ref(),
                                        )
                                        .map(|mut result| {
                                            if result.rtt_ms.is_none() {
                                                result.rtt_ms =
                                                    Some(start.elapsed().as_millis() as u64);
                                            }
                                            result
                                        })
                                        .map_err(|error| error.to_string())
                                })
                                .await
                        }
                    })
                },
                move || {
                    let cleanup_app_state = cleanup_app_state.clone();
                    let cleanup_detached_hook_scope = cleanup_detached_hook_scope.clone();
                    let cleanup_cancel_token = cleanup_cancel_token.clone();
                    let mut cleanup_cx = cleanup_cx.clone();

                    Box::pin(async move {
                        cleanup_cancel_token.cancel();
                        cleanup_detached_hook_scope
                            .cancel_and_wait(cleanup_app_state, &mut cleanup_cx)
                            .await
                            .map_err(|error| {
                                format_detached_hook_cleanup_failure(
                                    profile_id,
                                    &profile_name_for_cleanup,
                                    &error,
                                )
                            })
                    })
                },
                hook_context,
            )
            .await;

            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    match result {
                        Ok(result) => {
                            info!("Test connection successful for {}", profile_name);
                            this.test_status = if result.warnings.is_empty() {
                                TestStatus::Success
                            } else {
                                TestStatus::SuccessWithWarning
                            };
                            this.test_error =
                                (!result.warnings.is_empty()).then(|| result.warnings.join("\n"));
                            this.test_result = Some(result.test_result);
                        }
                        Err(error) => {
                            info!("Test connection failed: {}", error);
                            this.test_status = TestStatus::Failed;
                            this.test_error = Some(error);
                            this.test_result = None;
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }
}

type TestConnectionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

type TestConnectionDropGuard = Arc<Mutex<Vec<&'static str>>>;

#[derive(Default)]
struct TestConnectionProbeDropGuards {
    connection: Option<TestConnectionDropGuard>,
    access_handle: Option<TestConnectionDropGuard>,
}

struct TestConnectionProbeResource {
    name: &'static str,
    drop_guard: Option<TestConnectionDropGuard>,
}

impl TestConnectionProbeResource {
    #[cfg(test)]
    fn new(name: &'static str, drop_guard: Option<TestConnectionDropGuard>) -> Self {
        Self { name, drop_guard }
    }
}

impl Drop for TestConnectionProbeResource {
    #[expect(
        clippy::expect_used,
        reason = "`Drop` cannot return an error, so the existing \
                  panic-on-poisoned-mutex policy is retained here; the guard \
                  is only locked for a single `push`"
    )]
    fn drop(&mut self) {
        if let Some(drop_guard) = &self.drop_guard {
            drop_guard
                .lock()
                .expect("drop log poisoned")
                .push(self.name);
        }
    }
}

struct TestConnectionOrchestrationResult {
    test_result: datatab_core::TestConnectionResult,
    warnings: Vec<String>,
}

async fn run_test_connection_orchestration<'a, RunPhase, RunProbe, RunCleanup>(
    hooks: datatab_core::ConnectionHooks,
    run_phase: RunPhase,
    run_probe: RunProbe,
    run_cleanup: RunCleanup,
    hook_context: datatab_core::HookContext,
) -> Result<TestConnectionOrchestrationResult, String>
where
    RunPhase: FnMut(
        HookPhase,
        Vec<datatab_core::ConnectionHook>,
        datatab_core::HookContext,
    ) -> TestConnectionFuture<'a, HookPhaseState>,
    RunProbe:
        FnOnce(
            TestConnectionProbeDropGuards,
        )
            -> TestConnectionFuture<'a, Result<datatab_core::TestConnectionResult, String>>,
    RunCleanup: FnOnce() -> TestConnectionFuture<'a, Result<(), String>>,
{
    run_test_connection_orchestration_with_drop_guards(
        hooks,
        run_phase,
        run_probe,
        run_cleanup,
        hook_context,
        TestConnectionProbeDropGuards::default(),
    )
    .await
}

async fn run_test_connection_orchestration_with_drop_guards<'a, RunPhase, RunProbe, RunCleanup>(
    hooks: datatab_core::ConnectionHooks,
    mut run_phase: RunPhase,
    run_probe: RunProbe,
    run_cleanup: RunCleanup,
    hook_context: datatab_core::HookContext,
    drop_guards: TestConnectionProbeDropGuards,
) -> Result<TestConnectionOrchestrationResult, String>
where
    RunPhase: FnMut(
        HookPhase,
        Vec<datatab_core::ConnectionHook>,
        datatab_core::HookContext,
    ) -> TestConnectionFuture<'a, HookPhaseState>,
    RunProbe:
        FnOnce(
            TestConnectionProbeDropGuards,
        )
            -> TestConnectionFuture<'a, Result<datatab_core::TestConnectionResult, String>>,
    RunCleanup: FnOnce() -> TestConnectionFuture<'a, Result<(), String>>,
{
    let outcome =
        run_test_connection_phases(hooks, &mut run_phase, run_probe, hook_context, drop_guards)
            .await;
    let cleanup = run_cleanup().await;

    match (outcome, cleanup) {
        (Ok(result), Ok(())) => Ok(result),
        (Ok(_), Err(cleanup_error)) => Err(datatab_i18n::t!(
            "form.error.test_cleanup_failed",
            error = cleanup_error
        )),
        (Err(primary_error), Ok(())) => Err(primary_error),
        (Err(primary_error), Err(cleanup_error)) => Err(datatab_i18n::t!(
            "form.error.test_cleanup_warning",
            primary_error = primary_error,
            cleanup_error = cleanup_error
        )),
    }
}

async fn run_test_connection_phases<'a, RunPhase, RunProbe>(
    hooks: datatab_core::ConnectionHooks,
    run_phase: &mut RunPhase,
    run_probe: RunProbe,
    hook_context: datatab_core::HookContext,
    drop_guards: TestConnectionProbeDropGuards,
) -> Result<TestConnectionOrchestrationResult, String>
where
    RunPhase: FnMut(
        HookPhase,
        Vec<datatab_core::ConnectionHook>,
        datatab_core::HookContext,
    ) -> TestConnectionFuture<'a, HookPhaseState>,
    RunProbe:
        FnOnce(
            TestConnectionProbeDropGuards,
        )
            -> TestConnectionFuture<'a, Result<datatab_core::TestConnectionResult, String>>,
{
    let pre_connect = run_phase(
        HookPhase::PreConnect,
        hooks.pre_connect,
        hook_context.clone(),
    )
    .await;
    let mut warnings = match pre_connect {
        HookPhaseState::Continue { warnings } => warnings,
        HookPhaseState::Aborted { error } => return Err(error),
        HookPhaseState::Cancelled => {
            return Err(datatab_i18n::t!("form.error.test_cancelled_pre"));
        }
    };

    let result = run_probe(drop_guards).await?;

    let post_connect = run_phase(HookPhase::PostConnect, hooks.post_connect, hook_context).await;
    match post_connect {
        HookPhaseState::Continue {
            warnings: post_connect_warnings,
        } => {
            warnings.extend(post_connect_warnings);
            Ok(TestConnectionOrchestrationResult {
                test_result: result,
                warnings,
            })
        }
        HookPhaseState::Aborted { error } => Err(error),
        HookPhaseState::Cancelled => Err(datatab_i18n::t!("form.error.test_cancelled_post")),
    }
}

fn format_detached_hook_cleanup_failure(
    profile_id: uuid::Uuid,
    profile_name: &str,
    error: &datatab_ui_base::hook_phase_runner::DetachedHookCleanupError,
) -> String {
    let task_ids = error
        .task_ids()
        .iter()
        .map(uuid::Uuid::to_string)
        .collect::<Vec<_>>()
        .join(", ");

    datatab_i18n::t!(
        "form.error.detached_hook_cleanup_failed",
        profile_name = profile_name,
        profile_id = profile_id.to_string(),
        task_ids = task_ids,
        error = error.source().to_string()
    )
}

#[cfg(test)]
mod tests {
    mod import_auth_choice;
    mod managed_access;
    mod refusal;
    mod save_async;
    mod secret_fields;
    mod tls;
    mod value_refs;

    use super::*;
    use crate::connection_manager::{EditState, FormFocus};
    use crate::connection_manager::{KEYRING_PASSWORD_READ_ERROR, KEYRING_SECRETS_READ_ERROR};
    use datatab_core::LogErr;
    use datatab_core::secrecy::{ExposeSecret, SecretString};
    use datatab_core::{
        ConnectionHook, ConnectionHooks, ConnectionProfile, DbConfig, DbDriver, HookExecutionMode,
        HookFailureMode, HookKind, SecretStore,
    };
    use datatab_storage::bootstrap::StorageRuntime;
    use datatab_ui_base::AppStateEntity;
    use datatab_ui_base::toast::{ToastGlobal, ToastHost};
    use gpui::{Entity, TestAppContext, WindowHandle, WindowOptions};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    #[::core::prelude::v1::test]
    fn missing_bound_auth_profile_is_reported() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let app_state = test_app_state(
            &mut cx,
            SecretStoreFixture::new(PasswordSaveOutcome::Success),
        );
        let window = open_new_profile_window(app_state, &mut cx);
        let missing_id = uuid::Uuid::new_v4();
        window
            .update(&mut cx, |manager, _, cx| {
                manager.auth_profile.selected_auth_profile_id = Some(missing_id);
                assert!(!manager.validate_form(false, cx));
                assert!(manager.validation_errors.contains(&datatab_i18n::t!(
                    "form.validation.auth_profile_not_found",
                    id = missing_id.to_string()
                )));
            })
            .expect("validate form");
        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close window");
    }

    const FORM_KEYS: &[&str] = &[
        "form.validation.connection_name_required",
        "form.validation.connection_deleted_while_saving",
        "form.validation.deleted_connection_secret_cleanup_failed",
        "form.validation.no_driver_selected",
        "form.validation.field_required",
        "form.validation.field_invalid_number",
        "form.validation.ssh_host_required",
        "form.validation.ssh_user_required",
        "form.validation.ssh_port_invalid",
        "form.validation.auth_profile_not_found",
        "form.validation.dynamic_auth_profile_required",
        "form.validation.auth_profile_no_provider",
        "form.action.updating",
        "form.action.saving",
        "form.warning.refresh_interval_invalid",
        "form.error.build_profile_failed",
        "form.error.test_cancelled_pre",
        "form.error.test_cancelled_post",
        "form.error.test_cleanup_failed",
        "form.error.test_cleanup_warning",
        "form.error.detached_hook_cleanup_failed",
        "form.error.pipeline_stage_failed",
    ];

    #[::core::prelude::v1::test]
    fn form_validation_keys_resolve() {
        for key in FORM_KEYS {
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

    fn hook(command: &str) -> ConnectionHook {
        ConnectionHook {
            enabled: true,
            kind: HookKind::Command {
                command: command.to_string(),
                args: Vec::new(),
            },
            cwd: None,
            env: HashMap::new(),
            inherit_env: true,
            env_denylist: Vec::new(),
            timeout_ms: None,
            execution_mode: HookExecutionMode::Blocking,
            ready_signal: None,
            on_failure: HookFailureMode::Disconnect,
        }
    }

    fn current_unsaved_hook_context() -> datatab_core::HookContext {
        datatab_core::HookContext {
            profile_id: uuid::Uuid::from_u128(0x295),
            profile_name: "current unsaved profile".to_string(),
            db_kind: "postgres".to_string(),
            host: None,
            port: None,
            database: None,
            phase: None,
            dials_configured_endpoint: false,
        }
    }

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        use std::sync::Arc;
        use std::task::{Context, Poll, Wake, Waker};

        struct NoopWaker;
        impl Wake for NoopWaker {
            fn wake(self: Arc<Self>) {}
        }

        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);

        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test callback must complete without waiting"),
        }
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_passes_current_unsaved_context_and_returns_only_test_result() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let hooks = ConnectionHooks {
            pre_connect: vec![hook("current-pre")],
            post_connect: vec![hook("current-post")],
            ..Default::default()
        };

        let result = block_on(run_test_connection_orchestration(
            hooks,
            |phase, hooks, context| {
                let calls = calls.clone();
                let contexts = contexts.clone();
                Box::pin(async move {
                    calls
                        .lock()
                        .expect("call log poisoned")
                        .push((phase, hooks[0].display_command()));
                    contexts.lock().expect("context log poisoned").push(context);
                    HookPhaseState::Continue {
                        warnings: Vec::new(),
                    }
                })
            },
            |_| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls
                        .lock()
                        .expect("call log poisoned")
                        .push((HookPhase::PreConnect, "direct-probe".to_string()));
                    Ok(datatab_core::TestConnectionResult {
                        engine: Some("direct probe".to_string()),
                        ..Default::default()
                    })
                })
            },
            || Box::pin(async { Ok(()) }),
            current_unsaved_hook_context(),
        ));

        assert_eq!(
            result
                .expect("direct probe succeeds")
                .test_result
                .engine
                .as_deref(),
            Some("direct probe"),
        );
        assert_eq!(
            *calls.lock().expect("call log poisoned"),
            vec![
                (HookPhase::PreConnect, "current-pre".to_string()),
                (HookPhase::PreConnect, "direct-probe".to_string()),
                (HookPhase::PostConnect, "current-post".to_string()),
            ],
        );
        let contexts = contexts.lock().expect("context log poisoned");
        assert_eq!(contexts.len(), 2);
        for context in contexts.iter() {
            assert_eq!(context.profile_id, uuid::Uuid::from_u128(0x295));
            assert_eq!(context.profile_name, "current unsaved profile");
            assert_eq!(context.db_kind, "postgres");
        }
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_runs_direct_and_pipeline_success_through_connect_phases_once()
    {
        for probe_name in ["direct-probe", "pipeline-probe"] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let cleanup_calls = Arc::new(Mutex::new(0));
            let hooks = ConnectionHooks {
                pre_connect: vec![hook("pre-connect")],
                post_connect: vec![hook("post-connect")],
                pre_disconnect: vec![hook("must-not-run")],
                post_disconnect: vec![hook("must-not-run")],
            };

            let result = block_on(run_test_connection_orchestration(
                hooks,
                |phase, hooks, _| {
                    let calls = calls.clone();
                    Box::pin(async move {
                        calls
                            .lock()
                            .expect("call log poisoned")
                            .push((phase, hooks[0].display_command()));
                        HookPhaseState::Continue {
                            warnings: Vec::new(),
                        }
                    })
                },
                |_| {
                    let calls = calls.clone();
                    Box::pin(async move {
                        calls
                            .lock()
                            .expect("call log poisoned")
                            .push((HookPhase::PreConnect, probe_name.to_string()));
                        Ok(datatab_core::TestConnectionResult {
                            engine: Some(probe_name.to_string()),
                            ..Default::default()
                        })
                    })
                },
                || {
                    let cleanup_calls = cleanup_calls.clone();
                    Box::pin(async move {
                        *cleanup_calls.lock().expect("cleanup log poisoned") += 1;
                        Ok(())
                    })
                },
                current_unsaved_hook_context(),
            ));

            assert_eq!(
                result
                    .expect("successful probe publishes one result")
                    .test_result
                    .engine
                    .as_deref(),
                Some(probe_name),
            );
            assert_eq!(
                *calls.lock().expect("call log poisoned"),
                vec![
                    (HookPhase::PreConnect, "pre-connect".to_string()),
                    (HookPhase::PreConnect, probe_name.to_string()),
                    (HookPhase::PostConnect, "post-connect".to_string()),
                ],
                "Test Connection must not invoke disconnect phases",
            );
            assert_eq!(
                *cleanup_calls.lock().expect("cleanup log poisoned"),
                1,
                "each terminal outcome performs one scoped cleanup",
            );
        }
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_cleans_up_after_pre_hook_abort_without_probe_or_disconnect() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cleanup_calls = Arc::new(Mutex::new(0));
        let hooks = ConnectionHooks {
            pre_connect: vec![hook("pre-abort")],
            post_connect: vec![hook("must-not-run")],
            pre_disconnect: vec![hook("must-not-run")],
            post_disconnect: vec![hook("must-not-run")],
        };

        let result = block_on(run_test_connection_orchestration(
            hooks,
            |phase, hooks, _| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls
                        .lock()
                        .expect("call log poisoned")
                        .push((phase, hooks[0].display_command()));
                    HookPhaseState::Aborted {
                        error: "pre-connect Disconnect policy failed".to_string(),
                    }
                })
            },
            |_| Box::pin(async { panic!("pre-hook abort must skip probe") }),
            || {
                let cleanup_calls = cleanup_calls.clone();
                Box::pin(async move {
                    *cleanup_calls.lock().expect("cleanup log poisoned") += 1;
                    Ok(())
                })
            },
            current_unsaved_hook_context(),
        ));

        assert!(matches!(result, Err(error) if error == "pre-connect Disconnect policy failed"));
        assert_eq!(
            *calls.lock().expect("call log poisoned"),
            vec![(HookPhase::PreConnect, "pre-abort".to_string())],
        );
        assert_eq!(*cleanup_calls.lock().expect("cleanup log poisoned"), 1);
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_cleans_up_after_post_hook_disconnect_without_disconnect_phases()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cleanup_calls = Arc::new(Mutex::new(0));
        let hooks = ConnectionHooks {
            pre_connect: vec![hook("pre-connect")],
            post_connect: vec![hook("post-disconnect")],
            pre_disconnect: vec![hook("must-not-run")],
            post_disconnect: vec![hook("must-not-run")],
        };

        let result = block_on(run_test_connection_orchestration(
            hooks,
            |phase, hooks, _| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls
                        .lock()
                        .expect("call log poisoned")
                        .push((phase, hooks[0].display_command()));
                    match phase {
                        HookPhase::PreConnect => HookPhaseState::Continue {
                            warnings: Vec::new(),
                        },
                        HookPhase::PostConnect => HookPhaseState::Aborted {
                            error: "post-connect Disconnect policy failed".to_string(),
                        },
                        HookPhase::PreDisconnect | HookPhase::PostDisconnect => {
                            panic!("Test Connection must not run disconnect phases")
                        }
                    }
                })
            },
            |_| {
                Box::pin(async {
                    Ok(datatab_core::TestConnectionResult {
                        engine: Some("reachable".to_string()),
                        ..Default::default()
                    })
                })
            },
            || {
                let cleanup_calls = cleanup_calls.clone();
                Box::pin(async move {
                    *cleanup_calls.lock().expect("cleanup log poisoned") += 1;
                    Ok(())
                })
            },
            current_unsaved_hook_context(),
        ));

        assert!(matches!(result, Err(error) if error == "post-connect Disconnect policy failed"));
        assert_eq!(
            *calls.lock().expect("call log poisoned"),
            vec![
                (HookPhase::PreConnect, "pre-connect".to_string()),
                (HookPhase::PostConnect, "post-disconnect".to_string()),
            ],
        );
        assert_eq!(*cleanup_calls.lock().expect("cleanup log poisoned"), 1);
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_stops_after_failed_pipeline_probe() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let hooks = ConnectionHooks {
            pre_connect: vec![hook("pipeline-pre")],
            post_connect: vec![hook("pipeline-post")],
            pre_disconnect: vec![hook("must-not-run")],
            post_disconnect: vec![hook("must-not-run")],
        };

        let result = block_on(run_test_connection_orchestration(
            hooks,
            |phase, hooks, _| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls
                        .lock()
                        .expect("call log poisoned")
                        .push((phase, hooks[0].display_command()));
                    HookPhaseState::Continue {
                        warnings: Vec::new(),
                    }
                })
            },
            |_| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls
                        .lock()
                        .expect("call log poisoned")
                        .push((HookPhase::PreConnect, "pipeline-probe".to_string()));
                    Err("pipeline probe failed".to_string())
                })
            },
            || Box::pin(async { Ok(()) }),
            current_unsaved_hook_context(),
        ));

        assert!(matches!(result, Err(error) if error == "pipeline probe failed"));
        assert_eq!(
            *calls.lock().expect("call log poisoned"),
            vec![
                (HookPhase::PreConnect, "pipeline-pre".to_string()),
                (HookPhase::PreConnect, "pipeline-probe".to_string()),
            ],
        );
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_preserves_probe_failure_when_cleanup_fails() {
        let cleanup_calls = Arc::new(Mutex::new(0));

        let result = block_on(run_test_connection_orchestration(
            ConnectionHooks::default(),
            |_phase, _hooks, _context| {
                Box::pin(async move {
                    HookPhaseState::Continue {
                        warnings: Vec::new(),
                    }
                })
            },
            |_| Box::pin(async { Err("driver probe failed".to_string()) }),
            || {
                let cleanup_calls = cleanup_calls.clone();
                Box::pin(async move {
                    *cleanup_calls.lock().expect("cleanup log poisoned") += 1;
                    Err("detached hook cleanup failed".to_string())
                })
            },
            current_unsaved_hook_context(),
        ));

        let expected = datatab_i18n::t!(
            "form.error.test_cleanup_warning",
            primary_error = "driver probe failed",
            cleanup_error = "detached hook cleanup failed"
        );
        assert!(matches!(result, Err(error) if error == expected));
        assert_eq!(*cleanup_calls.lock().expect("cleanup log poisoned"), 1);
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_returns_hook_warnings_after_cleanup() {
        let cleanup_calls = Arc::new(Mutex::new(0));

        let result = block_on(run_test_connection_orchestration(
            ConnectionHooks::default(),
            |phase, _hooks, _context| {
                Box::pin(async move {
                    HookPhaseState::Continue {
                        warnings: vec![format!("{} hook warning", phase.label())],
                    }
                })
            },
            |_| {
                Box::pin(async {
                    Ok(datatab_core::TestConnectionResult {
                        engine: Some("reachable".to_string()),
                        ..Default::default()
                    })
                })
            },
            || {
                let cleanup_calls = cleanup_calls.clone();
                Box::pin(async move {
                    *cleanup_calls.lock().expect("cleanup log poisoned") += 1;
                    Ok(())
                })
            },
            current_unsaved_hook_context(),
        ));

        let result = result.expect("warnings do not fail the test");
        assert_eq!(result.test_result.engine.as_deref(), Some("reachable"));
        assert_eq!(
            result.warnings,
            vec!["Pre-connect hook warning", "Post-connect hook warning"],
        );
        assert_eq!(*cleanup_calls.lock().expect("cleanup log poisoned"), 1);
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_returns_post_hook_warn_but_not_ignore_as_warning() {
        for (post_hook_warnings, expected_warnings) in [
            (
                vec!["post-connect Warn policy failed".to_string()],
                vec!["post-connect Warn policy failed"],
            ),
            (Vec::new(), Vec::new()),
        ] {
            let phase_calls = Arc::new(Mutex::new(0));
            let cleanup_calls = Arc::new(Mutex::new(0));

            let result = block_on(run_test_connection_orchestration(
                ConnectionHooks::default(),
                |_phase, _hooks, _context| {
                    let phase_calls = phase_calls.clone();
                    let post_hook_warnings = post_hook_warnings.clone();
                    Box::pin(async move {
                        let mut phase_calls = phase_calls.lock().expect("phase log poisoned");
                        *phase_calls += 1;

                        if *phase_calls == 1 {
                            HookPhaseState::Continue {
                                warnings: Vec::new(),
                            }
                        } else {
                            HookPhaseState::Continue {
                                warnings: post_hook_warnings,
                            }
                        }
                    })
                },
                |_| {
                    Box::pin(async {
                        Ok(datatab_core::TestConnectionResult {
                            engine: Some("reachable".to_string()),
                            ..Default::default()
                        })
                    })
                },
                || {
                    let cleanup_calls = cleanup_calls.clone();
                    Box::pin(async move {
                        *cleanup_calls.lock().expect("cleanup log poisoned") += 1;
                        Ok(())
                    })
                },
                current_unsaved_hook_context(),
            ));

            let result = result.expect("Warn and Ignore post hooks preserve successful probe");
            assert_eq!(result.test_result.engine.as_deref(), Some("reachable"));
            assert_eq!(result.warnings, expected_warnings);
            assert_eq!(*phase_calls.lock().expect("phase log poisoned"), 2);
            assert_eq!(*cleanup_calls.lock().expect("cleanup log poisoned"), 1);
        }
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_cleans_up_after_cancelled_hook() {
        let cleanup_calls = Arc::new(Mutex::new(0));

        let result = block_on(run_test_connection_orchestration(
            ConnectionHooks::default(),
            |_phase, _hooks, _context| Box::pin(async { HookPhaseState::Cancelled }),
            |_| Box::pin(async { panic!("cancelled pre-connect hook must skip probe") }),
            || {
                let cleanup_calls = cleanup_calls.clone();
                Box::pin(async move {
                    *cleanup_calls.lock().expect("cleanup log poisoned") += 1;
                    Ok(())
                })
            },
            current_unsaved_hook_context(),
        ));

        let expected = datatab_i18n::t!("form.error.test_cancelled_pre");
        assert!(matches!(result, Err(error) if error == expected));
        assert_eq!(*cleanup_calls.lock().expect("cleanup log poisoned"), 1);
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_fails_successful_probe_when_cleanup_fails() {
        let result = block_on(run_test_connection_orchestration(
            ConnectionHooks::default(),
            |_phase, _hooks, _context| {
                Box::pin(async move {
                    HookPhaseState::Continue {
                        warnings: Vec::new(),
                    }
                })
            },
            |_| Box::pin(async { Ok(datatab_core::TestConnectionResult::default()) }),
            || Box::pin(async { Err("access handle did not close".to_string()) }),
            current_unsaved_hook_context(),
        ));

        let expected = datatab_i18n::t!(
            "form.error.test_cleanup_failed",
            error = "access handle did not close"
        );
        assert!(matches!(result, Err(error) if error == expected));
    }

    #[::core::prelude::v1::test]
    fn test_connection_orchestration_drops_probe_connection_and_access_handle_before_cleanup() {
        let drops = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let cleanup_observed_drops = drops.clone();

        let result = block_on(run_test_connection_orchestration_with_drop_guards(
            ConnectionHooks::default(),
            |_phase, _hooks, _context| {
                Box::pin(async move {
                    HookPhaseState::Continue {
                        warnings: Vec::new(),
                    }
                })
            },
            |drop_guards| {
                Box::pin(async move {
                    let connection = TestConnectionProbeResource::new(
                        "probe connection",
                        drop_guards.connection,
                    );
                    let access_handle = TestConnectionProbeResource::new(
                        "pipeline access handle",
                        drop_guards.access_handle,
                    );

                    drop(connection);
                    drop(access_handle);

                    Ok(datatab_core::TestConnectionResult::default())
                })
            },
            move || {
                Box::pin(async move {
                    assert_eq!(
                        *cleanup_observed_drops.lock().expect("drop log poisoned"),
                        vec!["probe connection", "pipeline access handle"],
                        "the actual probe resources must drop before cleanup starts",
                    );
                    Ok(())
                })
            },
            current_unsaved_hook_context(),
            TestConnectionProbeDropGuards {
                connection: Some(drops.clone()),
                access_handle: Some(drops),
            },
        ));

        assert!(result.is_ok(), "cleanup follows the dropped resources");
    }

    #[::core::prelude::v1::test]
    fn detached_hook_cleanup_failure_keeps_scope_and_source_context() {
        let task_id = uuid::Uuid::from_u128(0x2_500);
        let error = datatab_ui_base::hook_phase_runner::DetachedHookCleanupError::new(
            vec![task_id],
            "app state was released",
        );

        let message = format_detached_hook_cleanup_failure(
            uuid::Uuid::from_u128(0x295),
            "current unsaved profile",
            &error,
        );

        assert!(message.contains("current unsaved profile"));
        assert!(message.contains("00000000-0000-0000-0000-000000000295"));
        assert!(message.contains(&task_id.to_string()));
        assert!(message.contains("app state was released"));
    }

    const DRIVER_BUILD_ERROR: &str = "Profile and Access Key ID cannot both be set";

    /// Refuses either form construction or pure profile validation. With a
    /// `declared_kind` it refuses neither: like an RPC driver, it reports the
    /// kind its Hello declared and builds a generic config of that kind.
    struct RejectingDriver {
        inner: Arc<dyn DbDriver>,
        refuses_profile: bool,
        declared_kind: Option<datatab_core::DbKind>,
    }

    impl DbDriver for RejectingDriver {
        fn validate_profile(&self, _: &ConnectionProfile) -> Result<(), datatab_core::DbError> {
            if self.refuses_profile {
                Err(datatab_core::DbError::InvalidProfile(
                    "fixture refusal".into(),
                ))
            } else {
                Ok(())
            }
        }

        fn kind(&self) -> datatab_core::DbKind {
            self.declared_kind.unwrap_or_else(|| self.inner.kind())
        }

        fn metadata(&self) -> &datatab_core::DriverMetadata {
            self.inner.metadata()
        }

        fn form_definition(&self) -> &datatab_core::DriverFormDef {
            self.inner.form_definition()
        }

        fn driver_key(&self) -> datatab_core::DriverKey {
            self.inner.driver_key()
        }

        fn build_config(
            &self,
            values: &datatab_core::FormValues,
        ) -> Result<DbConfig, datatab_core::DbError> {
            if let Some(kind) = self.declared_kind {
                return Ok(DbConfig::External {
                    kind,
                    values: values.clone(),
                });
            }
            if self.refuses_profile {
                return self.inner.build_config(values);
            }
            Err(datatab_core::DbError::InvalidProfile(
                DRIVER_BUILD_ERROR.to_string(),
            ))
        }

        fn extract_values(&self, config: &DbConfig) -> datatab_core::FormValues {
            self.inner.extract_values(config)
        }

        fn connect_with_secrets(
            &self,
            profile: &ConnectionProfile,
            password: Option<&SecretString>,
            ssh_secret: Option<&SecretString>,
        ) -> Result<Box<dyn datatab_core::Connection>, datatab_core::DbError> {
            assert!(!self.refuses_profile, "refused form reached driver connect");
            self.inner
                .connect_with_secrets(profile, password, ssh_secret)
        }

        fn test_connection(
            &self,
            profile: &ConnectionProfile,
        ) -> Result<(), datatab_core::DbError> {
            self.inner.test_connection(profile)
        }
    }

    fn open_rejecting_driver_window(
        cx: &mut TestAppContext,
    ) -> (
        Entity<AppStateEntity>,
        WindowHandle<ConnectionManagerWindow>,
    ) {
        let app_state = test_app_state(cx, SecretStoreFixture::new(PasswordSaveOutcome::Success));
        let window = open_new_profile_window(app_state.clone(), cx);
        window
            .update(cx, |manager, _window, _cx| {
                let inner = manager
                    .form
                    .selected_driver
                    .clone()
                    .expect("SQLite driver selected");
                manager.form.selected_driver = Some(Arc::new(RejectingDriver {
                    inner,
                    refuses_profile: false,
                    declared_kind: None,
                }));
            })
            .expect("swap in the rejecting driver");
        (app_state, window)
    }

    fn shows_driver_build_error(manager: &ConnectionManagerWindow) -> bool {
        manager
            .validation_errors
            .iter()
            .chain(manager.test_error.iter())
            .any(|message| message.contains(DRIVER_BUILD_ERROR))
    }

    #[::core::prelude::v1::test]
    fn save_shows_the_driver_build_config_error() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let (app_state, window) = open_rejecting_driver_window(&mut cx);

        let shown = window
            .update(&mut cx, |manager, window, cx| {
                manager.save_profile(window, cx);
                shows_driver_build_error(manager)
            })
            .expect("save leaves the form window available");
        cx.run_until_parked();

        assert!(
            shown,
            "the driver's build_config error is visible after save"
        );
        assert!(
            cx.update(|cx| app_state.read(cx).profiles().is_empty()),
            "a rejected profile is not committed"
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn test_connection_shows_the_driver_build_config_error() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let (_app_state, window) = open_rejecting_driver_window(&mut cx);

        let shown = window
            .update(&mut cx, |manager, window, cx| {
                manager.test_connection(window, cx);
                shows_driver_build_error(manager)
            })
            .expect("test connection leaves the form window available");

        assert!(
            shown,
            "the driver's build_config error is visible after test connection"
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[derive(Clone, Copy)]
    enum PasswordSaveOutcome {
        Success,
        FailBeforeWrite,
        WriteThenFail,
    }

    #[derive(Clone)]
    struct SecretStoreFixture {
        outcome: Arc<Mutex<PasswordSaveOutcome>>,
        read_fails: Arc<AtomicBool>,
        /// A secret ref whose reads fail while every other read succeeds.
        failing_ref: Arc<Mutex<Option<String>>>,
        /// A secret ref whose writes fail while every other write follows
        /// `outcome`.
        failing_write_ref: Arc<Mutex<Option<String>>>,
        values: Arc<Mutex<HashMap<String, SecretString>>>,
        /// Every secret ref `get` was asked for.
        reads: Arc<Mutex<Vec<String>>>,
    }

    impl SecretStoreFixture {
        fn new(outcome: PasswordSaveOutcome) -> Self {
            Self {
                outcome: Arc::new(Mutex::new(outcome)),
                read_fails: Arc::new(AtomicBool::new(false)),
                failing_ref: Arc::new(Mutex::new(None)),
                failing_write_ref: Arc::new(Mutex::new(None)),
                values: Arc::new(Mutex::new(HashMap::new())),
                reads: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn reads_of(&self, secret_ref: &str) -> usize {
            self.reads
                .lock()
                .expect("test secret store reads lock poisoned")
                .iter()
                .filter(|read| read.as_str() == secret_ref)
                .count()
        }

        fn seed(&self, profile: &ConnectionProfile, password: &str) {
            self.values
                .lock()
                .expect("test secret store value lock poisoned")
                .insert(profile.secret_ref(), SecretString::from(password));
        }

        fn seed_ref(&self, secret_ref: &str, secret: &str) {
            self.values
                .lock()
                .expect("test secret store value lock poisoned")
                .insert(secret_ref.to_string(), SecretString::from(secret));
        }

        fn seed_ssh(&self, profile: &ConnectionProfile, secret: &str) {
            self.values
                .lock()
                .expect("test secret store value lock poisoned")
                .insert(profile.ssh_secret_ref(), SecretString::from(secret));
        }

        fn saved_ssh(&self, profile: &ConnectionProfile) -> Option<String> {
            self.values
                .lock()
                .expect("test secret store value lock poisoned")
                .get(&profile.ssh_secret_ref())
                .map(|secret| secret.expose_secret().to_string())
        }

        fn fail_reads_of(&self, secret_ref: String) {
            *self
                .failing_ref
                .lock()
                .expect("test secret store failing ref lock poisoned") = Some(secret_ref);
        }

        fn fail_writes_of(&self, secret_ref: String) {
            *self
                .failing_write_ref
                .lock()
                .expect("test secret store failing write ref lock poisoned") = Some(secret_ref);
        }

        fn saved(&self, profile: &ConnectionProfile) -> Option<String> {
            self.values
                .lock()
                .expect("test secret store value lock poisoned")
                .get(&profile.secret_ref())
                .map(|secret| secret.expose_secret().to_string())
        }
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "test fixture: retains the existing panic-on-poisoned-mutex \
                  policy in the `Result`-returning `SecretStore` methods; this \
                  fixture does not convert poisoned locks into returned errors"
    )]
    impl SecretStore for SecretStoreFixture {
        fn is_available(&self) -> bool {
            true
        }

        fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, datatab_core::DbError> {
            self.reads
                .lock()
                .expect("test secret store reads lock poisoned")
                .push(secret_ref.to_string());
            let failing_ref = self
                .failing_ref
                .lock()
                .expect("test secret store failing ref lock poisoned")
                .clone();
            if self.read_fails.load(Ordering::SeqCst) || failing_ref.as_deref() == Some(secret_ref)
            {
                return Err(datatab_core::DbError::IoError(std::io::Error::other(
                    "Cannot get secret of a locked object",
                )));
            }

            Ok(self
                .values
                .lock()
                .expect("test secret store value lock poisoned")
                .get(secret_ref)
                .cloned())
        }

        fn set(&self, secret_ref: &str, value: &SecretString) -> Result<(), datatab_core::DbError> {
            if self
                .failing_write_ref
                .lock()
                .expect("test secret store failing write ref lock poisoned")
                .as_deref()
                == Some(secret_ref)
            {
                return Err(datatab_core::DbError::IoError(std::io::Error::other(
                    "test keyring write failure for one ref",
                )));
            }
            match *self
                .outcome
                .lock()
                .expect("test secret store outcome lock poisoned")
            {
                PasswordSaveOutcome::Success => {
                    self.values
                        .lock()
                        .expect("test secret store value lock poisoned")
                        .insert(secret_ref.to_string(), value.clone());
                    Ok(())
                }
                PasswordSaveOutcome::FailBeforeWrite => Err(datatab_core::DbError::IoError(
                    std::io::Error::other("test keyring pre-write failure"),
                )),
                PasswordSaveOutcome::WriteThenFail => {
                    self.values
                        .lock()
                        .expect("test secret store value lock poisoned")
                        .insert(secret_ref.to_string(), value.clone());
                    Err(datatab_core::DbError::IoError(std::io::Error::other(
                        "test keyring write may have persisted",
                    )))
                }
            }
        }

        fn delete(&self, secret_ref: &str) -> Result<(), datatab_core::DbError> {
            self.values
                .lock()
                .expect("test secret store value lock poisoned")
                .remove(secret_ref);
            Ok(())
        }
    }

    fn init_form_test_runtime(cx: &mut TestAppContext) -> Entity<ToastHost> {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        cx.update(|cx| {
            let host = cx.new(|_| ToastHost::new());
            cx.set_global(ToastGlobal { host: host.clone() });
            host
        })
    }

    fn test_app_state(
        cx: &mut TestAppContext,
        fixture: SecretStoreFixture,
    ) -> Entity<AppStateEntity> {
        let app_state = cx.update(|cx| {
            cx.new(|_| {
                AppStateEntity::new_with_storage_runtime(
                    StorageRuntime::in_memory().expect("test storage runtime"),
                )
                .expect("test app state")
            })
        });
        app_state.update(cx, |state, _| {
            *state
                .secret_store()
                .write()
                .expect("test secret store lock poisoned") = Box::new(fixture);
        });
        app_state
    }

    fn sqlite_profile(name: &str) -> ConnectionProfile {
        let mut profile = ConnectionProfile::new(name, DbConfig::default_sqlite());
        profile.save_password = true;
        if let DbConfig::SQLite { path, .. } = &mut profile.config {
            *path = ":memory:".into();
        }
        profile
    }

    fn open_new_profile_window(
        app_state: Entity<AppStateEntity>,
        cx: &mut TestAppContext,
    ) -> WindowHandle<ConnectionManagerWindow> {
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| ConnectionManagerWindow::new(app_state, window, cx))
                })
            })
            .expect("connection manager window opens");
        window
            .update(cx, |manager, window, cx| {
                manager.select_driver("sqlite", window, cx);
                manager
                    .form
                    .input_name
                    .update(cx, |input, cx| input.set_value("new profile", window, cx));
                manager
                    .form
                    .driver_inputs
                    .get("path")
                    .expect("SQLite path input")
                    .update(cx, |input, cx| input.set_value(":memory:", window, cx));
                manager.form.input_password.update(cx, |input, cx| {
                    input.set_value("primary password", window, cx)
                });
            })
            .expect("new form initializes");
        window
    }

    fn open_edit_profile_window(
        app_state: Entity<AppStateEntity>,
        profile: ConnectionProfile,
        cx: &mut TestAppContext,
    ) -> WindowHandle<ConnectionManagerWindow> {
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| {
                        ConnectionManagerWindow::new_for_edit(app_state, &profile, window, cx)
                    })
                })
            })
            .expect("connection manager edit window opens");
        window
            .update(cx, |manager, window, cx| {
                manager.form.input_name.update(cx, |input, cx| {
                    input.set_value("edited profile", window, cx)
                });
                manager.form.input_password.update(cx, |input, cx| {
                    input.set_value("primary password", window, cx)
                });
            })
            .expect("edit form initializes");
        window
    }

    #[::core::prelude::v1::test]
    fn new_profile_password_save_failure_reports_once_and_keeps_the_window_open() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let app_state = test_app_state(
            &mut cx,
            SecretStoreFixture::new(PasswordSaveOutcome::FailBeforeWrite),
        );
        let window = open_new_profile_window(app_state.clone(), &mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                manager.save_profile(window, cx)
            })
            .expect("failed save leaves the real form window available");
        cx.run_until_parked();

        assert!(window.root(&mut cx).is_ok(), "the form window remains open");
        assert!(
            cx.update(|cx| app_state.read(cx).profiles().is_empty()),
            "a new profile is not committed"
        );
        assert_eq!(
            cx.update(|cx| host.read(cx).toast_count()),
            1,
            "one safe failure is reported"
        );
        assert_eq!(
            cx.update(|cx| host.read(cx).last_toast_title()),
            Some(PRIMARY_PASSWORD_SAVE_ERROR.to_string())
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn edited_profile_partial_password_write_does_not_commit_or_dismiss() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let app_state = test_app_state(
            &mut cx,
            SecretStoreFixture::new(PasswordSaveOutcome::WriteThenFail),
        );
        let profile = sqlite_profile("original profile");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        let window = open_edit_profile_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        window
            .update(&mut cx, |manager, window, cx| {
                manager.save_profile(window, cx)
            })
            .expect("partial-write failure leaves the real form window available");
        cx.run_until_parked();

        assert!(window.root(&mut cx).is_ok(), "the edit window remains open");
        let persisted = cx.update(|cx| app_state.read(cx).profiles().to_vec());
        assert_eq!(persisted.len(), 1, "no extra profile is committed");
        assert_eq!(
            persisted[0].name, profile.name,
            "the existing profile is not updated"
        );
        assert_eq!(
            cx.update(|cx| host.read(cx).toast_count()),
            1,
            "one safe failure is reported"
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    /// Opens the edit window without typing into the form, so the password
    /// input shows only what the keyring handed over.
    fn open_untouched_edit_window(
        app_state: Entity<AppStateEntity>,
        profile: ConnectionProfile,
        cx: &mut TestAppContext,
    ) -> WindowHandle<ConnectionManagerWindow> {
        cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| ConnectionManagerWindow::new_for_edit(app_state, &profile, window, cx))
            })
        })
        .expect("connection manager edit window opens")
    }

    #[::core::prelude::v1::test]
    fn editing_with_an_unreadable_keyring_reports_once_and_marks_the_password_unreadable() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("locked keyring profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        fixture.read_fails.store(true, Ordering::SeqCst);

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        let (stored_password, password_value, placeholder) = window
            .update(&mut cx, |manager, _window, cx| {
                (
                    manager.form.stored_password,
                    manager.form.input_password.read(cx).value().to_string(),
                    manager.password_placeholder(true, cx),
                )
            })
            .expect("edit window stays available");
        assert_eq!(stored_password, StoredPassword::Unreadable);
        assert_eq!(password_value, "", "the password input stays empty");
        assert_eq!(
            placeholder,
            datatab_i18n::t!("connection_manager.placeholder.password_unreadable")
        );
        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 1);
        assert_eq!(
            cx.update(|cx| host.read(cx).last_toast_title()),
            Some(KEYRING_PASSWORD_READ_ERROR.to_string())
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn editing_a_value_ref_profile_with_an_unreadable_keyring_reports_nothing() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let mut profile = sqlite_profile("value ref profile");
        profile.value_refs.insert(
            "password".to_string(),
            ValueRef::Env {
                key: "DATATAB_TEST_PASSWORD".to_string(),
            },
        );
        fixture.seed(&profile, "stale password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        fixture.read_fails.store(true, Ordering::SeqCst);

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        let stored_password = window
            .update(&mut cx, |manager, _window, _cx| {
                manager.form.stored_password
            })
            .expect("edit window stays available");
        assert_eq!(
            stored_password,
            StoredPassword::NotInKeyring,
            "saving still must not delete the entry the form never saw"
        );
        assert_eq!(fixture.reads_of(&profile.secret_ref()), 0);
        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 0);

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn saving_with_a_value_ref_source_keeps_an_entry_the_keyring_would_not_read() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("locked keyring profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        fixture.read_fails.store(true, Ordering::SeqCst);
        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();
        fixture.read_fails.store(false, Ordering::SeqCst);

        window
            .update(&mut cx, |manager, window, cx| {
                assert_eq!(manager.form.stored_password, StoredPassword::Unreadable);
                let env_ref = ValueRef::Env {
                    key: "DATATAB_TEST_PASSWORD".to_string(),
                };
                let env_key = manager
                    .form
                    .password_value_source_selector
                    .update(cx, |selector, cx| {
                        selector.set_value_ref(Some(&env_ref), window, cx)
                    });
                manager
                    .form
                    .input_password
                    .update(cx, |input, cx| input.set_value(env_key, window, cx));
                assert!(
                    !manager
                        .form
                        .password_value_source_selector
                        .read(cx)
                        .is_literal(cx)
                );
                manager.save_profile(window, cx)
            })
            .expect("save leaves the form window available");
        cx.run_until_parked();

        assert_eq!(
            fixture.saved(&profile).as_deref(),
            Some("saved password"),
            "the entry the form never saw is not deleted"
        );
        let persisted = cx.update(|cx| app_state.read(cx).profiles().to_vec());
        assert_eq!(persisted.len(), 1, "the edit is committed");
        assert!(
            matches!(
                persisted[0].value_refs.get("password"),
                Some(ValueRef::Env { key }) if key == "DATATAB_TEST_PASSWORD"
            ),
            "the new password source is saved: {:?}",
            persisted[0].value_refs.get("password")
        );
        assert!(
            window.root(&mut cx).is_err(),
            "a successful save closes the window"
        );
    }

    #[::core::prelude::v1::test]
    fn saving_with_save_password_unchecked_keeps_an_entry_the_keyring_would_not_read() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("locked keyring profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        fixture.read_fails.store(true, Ordering::SeqCst);
        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();
        fixture.read_fails.store(false, Ordering::SeqCst);

        window
            .update(&mut cx, |manager, window, cx| {
                assert_eq!(manager.form.stored_password, StoredPassword::Unreadable);
                manager.form.form_save_password = false;
                manager.form.input_name.update(cx, |input, cx| {
                    input.set_value("renamed profile", window, cx)
                });
                manager.save_profile(window, cx)
            })
            .expect("save leaves the form window available");
        cx.run_until_parked();

        assert_eq!(
            fixture.saved(&profile).as_deref(),
            Some("saved password"),
            "the entry the form never saw is not deleted"
        );
        let persisted = cx.update(|cx| app_state.read(cx).profiles().to_vec());
        assert_eq!(persisted.len(), 1);
        assert_eq!(
            persisted[0].name, "renamed profile",
            "the edit is committed"
        );
        assert!(!persisted[0].save_password);

        assert!(
            window.root(&mut cx).is_err(),
            "a successful save closes the window"
        );
    }

    #[::core::prelude::v1::test]
    fn saving_with_save_password_unchecked_deletes_an_entry_the_keyring_read() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("readable keyring profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        let (stored_password, password_value) = window
            .update(&mut cx, |manager, _window, cx| {
                (
                    manager.form.stored_password,
                    manager.form.input_password.read(cx).value().to_string(),
                )
            })
            .expect("edit window stays available");
        assert_eq!(stored_password, StoredPassword::Known);
        assert_eq!(password_value, "saved password");
        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 0);

        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.form_save_password = false;
                manager.save_profile(window, cx)
            })
            .expect("save leaves the form window available");
        cx.run_until_parked();

        assert_eq!(fixture.saved(&profile), None, "the entry is deleted");
        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 0);

        assert!(
            window.root(&mut cx).is_err(),
            "a successful save closes the window"
        );
    }

    #[::core::prelude::v1::test]
    fn editing_reads_the_keyring_only_when_the_password_lives_there() {
        let mut cx = TestAppContext::single();
        init_form_test_runtime(&mut cx);

        let mut env_profile = sqlite_profile("env password profile");
        env_profile.value_refs.insert(
            "password".to_string(),
            datatab_core::ValueRef::env("PGPASSWORD"),
        );
        let mut unsaved_profile = sqlite_profile("unsaved password profile");
        unsaved_profile.save_password = false;
        let keyring_profile = sqlite_profile("keyring password profile");

        for (profile, reads, expected) in [
            (env_profile, 0, StoredPassword::NotInKeyring),
            (unsaved_profile, 0, StoredPassword::NotInKeyring),
            (keyring_profile, 1, StoredPassword::Known),
        ] {
            let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
            let app_state = test_app_state(&mut cx, fixture.clone());
            fixture.seed(&profile, "saved password");
            app_state.update(&mut cx, |state, _| {
                state.add_profile_in_folder(profile.clone(), None)
            });

            let window = open_untouched_edit_window(app_state, profile.clone(), &mut cx);
            cx.run_until_parked();

            let stored_password = window
                .update(&mut cx, |manager, window, _| {
                    let stored = manager.form.stored_password;
                    window.remove_window();
                    stored
                })
                .expect("edit window stays available");
            assert_eq!(stored_password, expected, "{}", profile.name);
            assert_eq!(
                fixture.reads_of(&profile.secret_ref()),
                reads,
                "{}",
                profile.name
            );
            assert_eq!(
                fixture.saved(&profile).as_deref(),
                Some("saved password"),
                "the entry the form never read stays"
            );
        }
    }

    #[::core::prelude::v1::test]
    fn saving_a_form_with_the_password_not_in_the_keyring_keeps_the_saved_entry() {
        let mut cx = TestAppContext::single();
        init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let mut profile = sqlite_profile("unsaved password profile");
        profile.save_password = false;
        fixture.seed(&profile, "stale password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        let window = open_untouched_edit_window(app_state, profile.clone(), &mut cx);
        cx.run_until_parked();

        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.form_save_password = false;
                manager.save_profile(window, cx)
            })
            .expect("save leaves the form window available");
        cx.run_until_parked();

        assert_eq!(
            fixture.saved(&profile).as_deref(),
            Some("stale password"),
            "an entry the form never saw is not deleted"
        );
    }

    #[::core::prelude::v1::test]
    fn editing_with_no_saved_entry_reports_nothing_and_knows_the_password() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("no entry profile");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        let (stored_password, password_value) = window
            .update(&mut cx, |manager, _window, cx| {
                (
                    manager.form.stored_password,
                    manager.form.input_password.read(cx).value().to_string(),
                )
            })
            .expect("edit window stays available");
        assert_eq!(stored_password, StoredPassword::Known);
        assert_eq!(password_value, "");
        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 0);

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn saving_while_the_keyring_read_is_pending_keeps_the_saved_entry() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("pending read profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                assert_eq!(manager.form.stored_password, StoredPassword::Loading);
                assert_eq!(manager.form.input_password.read(cx).value(), "");
                manager.form.form_save_password = false;
                manager.save_profile(window, cx)
            })
            .expect("save leaves the form window available");

        assert_eq!(
            fixture.saved(&profile).as_deref(),
            Some("saved password"),
            "the entry the form had not read yet is not deleted"
        );
    }

    #[::core::prelude::v1::test]
    fn the_saved_password_fills_the_empty_input_when_the_keyring_read_completes() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("slow keyring profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        let (stored_password, password_value) = window
            .update(&mut cx, |manager, _window, cx| {
                (
                    manager.form.stored_password,
                    manager.form.input_password.read(cx).value().to_string(),
                )
            })
            .expect("edit window stays available");
        assert_eq!(stored_password, StoredPassword::Known);
        assert_eq!(password_value, "saved password");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    fn ssh_profile(name: &str) -> ConnectionProfile {
        let mut config = DbConfig::default_postgres();
        if let DbConfig::Postgres { ssh_tunnel, .. } = &mut config {
            *ssh_tunnel = Some(SshTunnelConfig {
                host: "bastion.example".to_string(),
                port: 22,
                user: "tunnel".to_string(),
                auth_method: datatab_core::SshAuthMethod::Password,
                ssh_config_host: None,
            });
        }
        let mut profile = ConnectionProfile::new(name, config);
        profile.save_password = true;
        profile
    }

    fn ssh_password_input(
        window: &WindowHandle<ConnectionManagerWindow>,
        cx: &mut TestAppContext,
    ) -> String {
        window
            .update(cx, |manager, _window, cx| {
                manager
                    .access
                    .input_ssh_password
                    .read(cx)
                    .value()
                    .to_string()
            })
            .expect("edit window stays available")
    }

    #[::core::prelude::v1::test]
    fn an_unreadable_inline_ssh_secret_survives_saving_with_save_unchecked() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = ssh_profile("locked ssh profile");
        fixture.seed(&profile, "saved password");
        fixture.seed_ssh(&profile, "saved ssh secret");
        fixture.fail_reads_of(profile.ssh_secret_ref());
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        window
            .update(&mut cx, |manager, window, cx| {
                assert_eq!(manager.form.stored_ssh_secret, StoredPassword::Unreadable);
                assert_eq!(manager.form.stored_password, StoredPassword::Known);
                manager.form.form_save_ssh_secret = false;
                manager.save_profile(window, cx)
            })
            .expect("save leaves the form window available");
        cx.run_until_parked();

        assert_eq!(
            fixture.saved_ssh(&profile).as_deref(),
            Some("saved ssh secret"),
            "the SSH entry the form never read is not deleted"
        );
    }

    #[::core::prelude::v1::test]
    fn saving_while_the_inline_ssh_read_is_pending_keeps_the_saved_entry() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = ssh_profile("pending ssh profile");
        fixture.seed_ssh(&profile, "saved ssh secret");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                assert_eq!(manager.form.stored_ssh_secret, StoredPassword::Loading);
                manager.form.form_save_ssh_secret = false;
                manager.save_profile(window, cx)
            })
            .expect("save leaves the form window available");

        assert_eq!(
            fixture.saved_ssh(&profile).as_deref(),
            Some("saved ssh secret")
        );
    }

    #[::core::prelude::v1::test]
    fn a_readable_inline_ssh_secret_fills_the_empty_input_when_the_read_completes() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = ssh_profile("readable ssh profile");
        fixture.seed_ssh(&profile, "saved ssh secret");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        assert_eq!(ssh_password_input(&window, &mut cx), "");
        cx.run_until_parked();

        assert_eq!(ssh_password_input(&window, &mut cx), "saved ssh secret");
        window
            .update(&mut cx, |manager, _window, _cx| {
                assert_eq!(manager.form.stored_ssh_secret, StoredPassword::Known);
                assert!(manager.form.form_save_ssh_secret);
            })
            .expect("edit window stays available");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn an_ssh_secret_typed_before_the_read_completes_is_kept() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = ssh_profile("typed ssh profile");
        fixture.seed_ssh(&profile, "saved ssh secret");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, window, cx| {
                manager.access.input_ssh_password.update(cx, |input, cx| {
                    input.set_value("typed ssh secret", window, cx)
                });
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        assert_eq!(ssh_password_input(&window, &mut cx), "typed ssh secret");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    fn window_is_closed(
        window: &WindowHandle<ConnectionManagerWindow>,
        cx: &mut TestAppContext,
    ) -> bool {
        window.root(cx).is_err()
    }

    fn postgres_profile(name: &str) -> ConnectionProfile {
        let mut profile = ConnectionProfile::new(name, DbConfig::default_postgres());
        profile.save_password = true;
        profile
    }

    #[::core::prelude::v1::test]
    fn saving_while_the_read_is_pending_runs_after_it_and_deletes_a_readable_entry() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("deferred save profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.form_save_password = false;
                manager.save_profile(window, cx)
            })
            .expect("edit window stays available");

        assert_eq!(fixture.saved(&profile).as_deref(), Some("saved password"));
        assert!(!window_is_closed(&window, &mut cx), "nothing ran yet");

        cx.run_until_parked();

        assert_eq!(
            fixture.saved(&profile),
            None,
            "the read showed the entry, so the user's opt-out deletes it"
        );
        assert!(window_is_closed(&window, &mut cx), "the save completed");
    }

    #[::core::prelude::v1::test]
    fn saving_while_the_read_is_pending_keeps_an_entry_the_locked_keyring_would_not_read() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("deferred locked save profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        fixture.read_fails.store(true, Ordering::SeqCst);

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.form_save_password = false;
                manager.save_profile(window, cx)
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        assert_eq!(fixture.saved(&profile).as_deref(), Some("saved password"));
        assert!(window_is_closed(&window, &mut cx), "the save completed");
    }

    #[::core::prelude::v1::test]
    fn test_connection_while_the_read_is_pending_starts_after_it() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("deferred test profile");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, window, cx| {
                manager.test_connection(window, cx);
                assert!(
                    matches!(manager.test_status, TestStatus::None),
                    "not started yet"
                );
                assert_eq!(
                    manager.deferred_after_secrets_read,
                    Some(DeferredAction::TestConnection)
                );
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        window
            .update(&mut cx, |manager, _window, _cx| {
                assert!(
                    !matches!(manager.test_status, TestStatus::None),
                    "the test ran"
                );
                assert_eq!(manager.deferred_after_secrets_read, None);
            })
            .expect("edit window stays available");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn a_save_request_outranks_a_test_request_while_the_read_is_pending() {
        assert_eq!(
            DeferredAction::merge(Some(DeferredAction::Save), DeferredAction::TestConnection),
            DeferredAction::Save
        );
        assert_eq!(
            DeferredAction::merge(Some(DeferredAction::TestConnection), DeferredAction::Save),
            DeferredAction::Save
        );
        assert_eq!(
            DeferredAction::merge(None, DeferredAction::TestConnection),
            DeferredAction::TestConnection
        );
    }

    #[::core::prelude::v1::test]
    fn a_password_source_switched_to_env_during_the_read_is_not_filled_or_saved() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("env switch profile");
        fixture.seed(&profile, "keyring secret");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, window, cx| {
                manager
                    .form
                    .password_value_source_selector
                    .update(cx, |selector, cx| {
                        selector.set_value_ref(
                            Some(&ValueRef::Env { key: String::new() }),
                            window,
                            cx,
                        );
                    });
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        window
            .update(&mut cx, |manager, window, cx| {
                assert_eq!(manager.form.input_password.read(cx).value(), "");
                manager.form.input_password.update(cx, |input, cx| {
                    input.set_value("DATATAB_TEST_PASSWORD", window, cx)
                });
                manager.save_profile(window, cx)
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        let saved = cx.update(|cx| app_state.read(cx).profiles().to_vec());
        let value_ref = saved[0]
            .value_refs
            .get("password")
            .expect("the password keeps a value ref");
        assert!(
            matches!(value_ref, ValueRef::Env { key } if key == "DATATAB_TEST_PASSWORD"),
            "got {value_ref:?}"
        );
        assert!(
            !format!("{:?}", saved[0].value_refs).contains("keyring secret"),
            "the keyring secret never reaches the profile"
        );
    }

    #[::core::prelude::v1::test]
    fn a_password_typed_then_cleared_during_the_read_stays_empty() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("cleared profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.input_password.update(cx, |input, cx| {
                    input.insert("x", window, cx);
                    input.replace_all("", window, cx);
                });
                assert_eq!(manager.form.input_password.read(cx).value(), "");
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        window
            .update(&mut cx, |manager, _window, cx| {
                assert_eq!(manager.form.stored_password, StoredPassword::Known);
                assert_eq!(manager.form.input_password.read(cx).value(), "");
            })
            .expect("edit window stays available");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn the_uri_shows_the_password_once_the_read_fills_it() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = postgres_profile("uri profile");
        fixture.seed(&profile, "hunter2");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        let uri = |cx: &mut TestAppContext| {
            window
                .update(cx, |manager, _window, cx| {
                    manager
                        .form
                        .driver_inputs
                        .get("uri")
                        .expect("URI input")
                        .read(cx)
                        .value()
                        .to_string()
                })
                .expect("edit window stays available")
        };
        assert!(!uri(&mut cx).contains("hunter2"));
        cx.run_until_parked();

        assert!(uri(&mut cx).contains("hunter2"), "uri: {}", uri(&mut cx));

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn an_ssh_method_changed_during_the_read_leaves_both_ssh_inputs_empty() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = ssh_profile("method switch profile");
        fixture.seed_ssh(&profile, "saved ssh secret");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, _window, _cx| {
                manager.access.ssh_auth_method = crate::ssh_shared::SshAuthSelection::PrivateKey;
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        window
            .update(&mut cx, |manager, _window, cx| {
                assert_eq!(manager.access.input_ssh_password.read(cx).value(), "");
                assert_eq!(manager.access.input_ssh_key_passphrase.read(cx).value(), "");
                assert_eq!(
                    manager.form.stored_ssh_secret,
                    StoredPassword::Unreadable,
                    "the entry was never shown, so saving must not delete it"
                );
            })
            .expect("edit window stays available");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn a_locked_keyring_reports_once_for_the_password_and_the_ssh_secret() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = ssh_profile("fully locked profile");
        fixture.seed(&profile, "saved password");
        fixture.seed_ssh(&profile, "saved ssh secret");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        fixture.read_fails.store(true, Ordering::SeqCst);

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 1);
        assert_eq!(
            cx.update(|cx| host.read(cx).last_toast_title()),
            Some(KEYRING_SECRETS_READ_ERROR.to_string())
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn a_locked_keyring_with_only_the_ssh_secret_failing_uses_the_secrets_summary() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = ssh_profile("ssh only locked profile");
        fixture.seed_ssh(&profile, "saved ssh secret");
        fixture.fail_reads_of(profile.ssh_secret_ref());
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        cx.run_until_parked();

        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 1);
        assert_eq!(
            cx.update(|cx| host.read(cx).last_toast_title()),
            Some(KEYRING_SECRETS_READ_ERROR.to_string())
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    /// A Postgres profile bound to a saved tunnel profile that keeps its secret.
    fn bound_tunnel_fixture(
        cx: &mut TestAppContext,
        fixture: &SecretStoreFixture,
    ) -> (
        Entity<AppStateEntity>,
        ConnectionProfile,
        datatab_core::SshTunnelProfile,
    ) {
        let app_state = test_app_state(cx, fixture.clone());
        let tunnel = datatab_core::SshTunnelProfile::new(
            "saved tunnel",
            SshTunnelConfig {
                host: "bastion.example".to_string(),
                port: 22,
                user: "tunnel".to_string(),
                auth_method: datatab_core::SshAuthMethod::Password,
                ssh_config_host: None,
            },
        );
        assert!(tunnel.save_secret);

        let mut profile = ConnectionProfile::new("bound profile", DbConfig::default_postgres());
        if let DbConfig::Postgres {
            ssh_tunnel_profile_id,
            ..
        } = &mut profile.config
        {
            *ssh_tunnel_profile_id = Some(tunnel.id);
        }
        profile.access_kind = Some(AccessKind::Ssh {
            ssh_tunnel_profile_id: tunnel.id,
        });

        app_state.update(cx, |state, _| {
            state.add_ssh_tunnel(tunnel.clone());
            state.add_profile_in_folder(profile.clone(), None)
        });
        (app_state, profile, tunnel)
    }

    #[::core::prelude::v1::test]
    fn a_bound_tunnel_secret_fills_the_empty_input_when_the_read_completes() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let (app_state, profile, tunnel) = bound_tunnel_fixture(&mut cx, &fixture);
        fixture.seed_ref(&tunnel.secret_ref(), "tunnel secret");

        let window = open_untouched_edit_window(app_state, profile, &mut cx);
        assert_eq!(ssh_password_input(&window, &mut cx), "");
        cx.run_until_parked();

        assert_eq!(ssh_password_input(&window, &mut cx), "tunnel secret");
        window
            .update(&mut cx, |manager, _window, _cx| {
                assert!(manager.form.form_save_ssh_secret);
            })
            .expect("edit window stays available");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn an_unreadable_bound_tunnel_secret_reports_once() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let (app_state, profile, tunnel) = bound_tunnel_fixture(&mut cx, &fixture);
        fixture.seed_ref(&tunnel.secret_ref(), "tunnel secret");
        fixture.fail_reads_of(tunnel.secret_ref());

        let window = open_untouched_edit_window(app_state, profile, &mut cx);
        cx.run_until_parked();

        assert_eq!(ssh_password_input(&window, &mut cx), "");
        assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 1);
        assert_eq!(
            cx.update(|cx| host.read(cx).last_toast_title()),
            Some(KEYRING_SECRETS_READ_ERROR.to_string())
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    #[::core::prelude::v1::test]
    fn a_password_typed_before_the_keyring_read_completes_is_kept() {
        let mut cx = TestAppContext::single();
        let _host = init_form_test_runtime(&mut cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(&mut cx, fixture.clone());
        let profile = sqlite_profile("typed first profile");
        fixture.seed(&profile, "saved password");
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.input_password.update(cx, |input, cx| {
                    input.set_value("typed password", window, cx)
                });
            })
            .expect("edit window stays available");
        cx.run_until_parked();

        let (stored_password, password_value) = window
            .update(&mut cx, |manager, _window, cx| {
                (
                    manager.form.stored_password,
                    manager.form.input_password.read(cx).value().to_string(),
                )
            })
            .expect("edit window stays available");
        assert_eq!(stored_password, StoredPassword::Known);
        assert_eq!(password_value, "typed password");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close the test window");
    }

    /// The Save button shows the keymap's Save shortcut, and pressing it
    /// saves from the form, whether a field is being edited or not.
    #[::core::prelude::v1::test]
    fn the_save_shortcut_saves_the_connection() {
        let mut cx = TestAppContext::single();
        init_form_test_runtime(&mut cx);
        cx.update(datatab_ui_base::keymap::init_keymap);
        let app_state = test_app_state(
            &mut cx,
            SecretStoreFixture::new(PasswordSaveOutcome::Success),
        );
        let window = open_new_profile_window(app_state.clone(), &mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                manager.edit_state = EditState::Navigating;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("the form focuses");
        cx.run_until_parked();

        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes(window.into(), "cmd-s");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes(window.into(), "ctrl-s");

        assert!(
            window.root(&mut cx).is_err(),
            "the shortcut saves and closes the form window"
        );
        assert!(
            cx.update(|cx| !app_state.read(cx).profiles().is_empty()),
            "the profile is persisted"
        );
    }

    /// Left and Right on "Enter as" switch between Fields and Connection URI
    /// instead of leaving the row.
    #[::core::prelude::v1::test]
    fn arrows_switch_enter_as_between_fields_and_uri() {
        let mut cx = TestAppContext::single();
        init_form_test_runtime(&mut cx);
        cx.update(datatab_ui_base::keymap::init_keymap);
        let app_state = test_app_state(
            &mut cx,
            SecretStoreFixture::new(PasswordSaveOutcome::Success),
        );
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| ConnectionManagerWindow::new(app_state, window, cx))
                })
            })
            .expect("connection manager window opens");

        window
            .update(&mut cx, |manager, window, cx| {
                manager.select_driver("postgres", window, cx);
                manager.edit_state = EditState::Navigating;
                manager.form_focus = FormFocus::UseUri;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("the form focuses");
        cx.run_until_parked();

        let uses_uri = |cx: &mut TestAppContext| {
            window
                .update(cx, |manager, _, _| {
                    manager
                        .form
                        .checkbox_states
                        .get("use_uri")
                        .copied()
                        .unwrap_or(false)
                })
                .expect("window is open")
        };

        cx.simulate_keystrokes(window.into(), "right");
        assert!(uses_uri(&mut cx), "Right picks Connection URI");
        assert_eq!(
            window
                .update(&mut cx, |manager, _, _| manager.form_focus)
                .expect("window is open"),
            FormFocus::UseUri,
            "the cursor stays on the field"
        );

        cx.simulate_keystrokes(window.into(), "left");
        assert!(!uses_uri(&mut cx), "Left picks Fields again");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    fn open_driver_picker(cx: &mut TestAppContext) -> WindowHandle<ConnectionManagerWindow> {
        init_form_test_runtime(cx);
        cx.update(datatab_ui_base::keymap::init_keymap);
        let app_state = test_app_state(cx, SecretStoreFixture::new(PasswordSaveOutcome::Success));

        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| ConnectionManagerWindow::new(app_state, window, cx))
                })
            })
            .expect("connection manager window opens");
        cx.run_until_parked();

        window
    }

    /// Letters bound to navigation in the connection manager reach a focused
    /// text field as text.
    #[::core::prelude::v1::test]
    fn navigation_letters_type_into_the_driver_filter() {
        let mut cx = TestAppContext::single();
        let window = open_driver_picker(&mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                manager
                    .form
                    .driver_filter_input
                    .update(cx, |input, cx| input.focus(window, cx));
            })
            .expect("the filter focuses");
        cx.run_until_parked();

        cx.simulate_keystrokes(window.into(), "j k h l /");

        let filter = window
            .update(&mut cx, |manager, _, cx| manager.current_driver_filter(cx))
            .expect("window is open");
        assert_eq!(filter, "jkhl/");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// With no text field focused, `j` and `k` still move through the drivers.
    #[::core::prelude::v1::test]
    fn navigation_letters_move_the_driver_selection_outside_text_fields() {
        let mut cx = TestAppContext::single();
        let window = open_driver_picker(&mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                window.focus(&manager.focus_handle, cx);
            })
            .expect("the picker focuses");
        cx.run_until_parked();

        let selected = |cx: &mut TestAppContext| {
            window
                .update(cx, |manager, _, _| manager.driver_focus.index())
                .expect("window is open")
        };

        cx.simulate_keystrokes(window.into(), "j");
        assert_ne!(selected(&mut cx), 0, "`j` moves the selection");

        cx.simulate_keystrokes(window.into(), "k");
        assert_eq!(selected(&mut cx), 0, "`k` moves it back");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// Opens a keymap-driven PostgreSQL form with the keyboard in the form.
    fn open_postgres_form(cx: &mut TestAppContext) -> WindowHandle<ConnectionManagerWindow> {
        open_postgres_form_with(cx, |_| {})
    }

    /// [`open_postgres_form`] after `seed` prepares the app state.
    fn open_postgres_form_with(
        cx: &mut TestAppContext,
        seed: impl FnOnce(&mut AppStateEntity),
    ) -> WindowHandle<ConnectionManagerWindow> {
        init_form_test_runtime(cx);
        cx.update(datatab_ui_base::keymap::init_keymap);
        let app_state = test_app_state(cx, SecretStoreFixture::new(PasswordSaveOutcome::Success));
        app_state.update(cx, |state, _| seed(state));
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| ConnectionManagerWindow::new(app_state, window, cx))
                })
            })
            .expect("connection manager window opens");

        window
            .update(cx, |manager, window, cx| {
                manager.select_driver("postgres", window, cx);
                manager.edit_state = EditState::Navigating;
                manager.form_focus = FormFocus::Name;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("the form focuses");
        cx.run_until_parked();

        window
    }

    fn form_state(
        window: WindowHandle<ConnectionManagerWindow>,
        cx: &mut TestAppContext,
    ) -> (FormFocus, EditState, super::super::ActiveTab) {
        window
            .update(cx, |manager, _, _| {
                (manager.form_focus, manager.edit_state, manager.active_tab)
            })
            .expect("window is open")
    }

    /// Down and Up move the form cursor like j and k, and while a field is
    /// edited they, Ctrl+L and Ctrl+H leave it: Down and Up for the next or
    /// previous field, Ctrl+L and Ctrl+H for the next or previous tab.
    #[::core::prelude::v1::test]
    fn arrows_and_tab_chords_move_on_from_the_form_and_its_fields() {
        let mut cx = TestAppContext::single();
        use super::super::ActiveTab;

        let window = open_postgres_form(&mut cx);

        cx.simulate_keystrokes(window.into(), "down");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::Environment);

        cx.simulate_keystrokes(window.into(), "up");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::Name);

        cx.simulate_keystrokes(window.into(), "enter");
        assert_eq!(form_state(window, &mut cx).1, EditState::Editing);

        cx.simulate_keystrokes(window.into(), "down");
        assert_eq!(
            form_state(window, &mut cx),
            (
                FormFocus::Environment,
                EditState::Navigating,
                ActiveTab::Main
            ),
            "Down leaves the name field for the next one"
        );

        cx.simulate_keystrokes(window.into(), "up enter");
        assert_eq!(form_state(window, &mut cx).1, EditState::Editing);

        cx.simulate_keystrokes(window.into(), "up");
        assert_eq!(
            form_state(window, &mut cx).1,
            EditState::Navigating,
            "Up leaves the field"
        );

        cx.simulate_keystrokes(window.into(), "down up enter ctrl-l");
        let (_, edit_state, tab) = form_state(window, &mut cx);
        assert_eq!(
            (edit_state, tab),
            (EditState::Navigating, ActiveTab::Access),
            "Ctrl+L leaves the field for the next tab"
        );

        window
            .update(&mut cx, |manager, window, cx| {
                manager.active_tab = ActiveTab::Main;
                manager.form_focus = FormFocus::Name;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("window is open");
        cx.simulate_keystrokes(window.into(), "enter ctrl-h");
        let (_, edit_state, tab) = form_state(window, &mut cx);
        assert_eq!(
            (edit_state, tab),
            (EditState::Navigating, ActiveTab::Mcp),
            "Ctrl+H leaves the field for the previous tab"
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// The ring reaches the transport section after the password: the SSL
    /// mode is a stop whose choice Left and Right change.
    #[::core::prelude::v1::test]
    fn the_form_ring_reaches_the_ssl_mode() {
        let mut cx = TestAppContext::single();
        use super::super::MainExtraStop;

        let window = open_postgres_form(&mut cx);

        let ssl_stop = window
            .update(&mut cx, |manager, window, cx| {
                manager.form_focus = FormFocus::TestConnection;
                window.focus(&manager.focus_handle, cx);
                manager.main_extra_focus_for_ssl_mode()
            })
            .expect("window is open")
            .expect("PostgreSQL offers SSL modes");

        cx.simulate_keystrokes(window.into(), "k");
        let stops = window
            .update(&mut cx, |manager, _, _| manager.main_extra_stops())
            .expect("window is open");
        assert_eq!(
            form_state(window, &mut cx).0,
            FormFocus::MainExtra((stops.len() - 1) as u8),
            "k from Test connection lands on the last extra stop"
        );

        window
            .update(&mut cx, |manager, _, _| manager.form_focus = ssl_stop)
            .expect("window is open");
        assert!(matches!(
            stops.get(match ssl_stop {
                FormFocus::MainExtra(index) => index as usize,
                _ => usize::MAX,
            }),
            Some(MainExtraStop::SslMode)
        ));

        let ssl_mode = |cx: &mut TestAppContext| {
            window
                .update(cx, |manager, _, _| manager.form.selected_ssl_mode.clone())
                .expect("window is open")
        };
        let before = ssl_mode(&mut cx);

        cx.simulate_keystrokes(window.into(), "right");
        assert_ne!(ssl_mode(&mut cx), before, "Right picks the next SSL mode");
        assert_eq!(form_state(window, &mut cx).0, ssl_stop, "the cursor stays");

        cx.simulate_keystrokes(window.into(), "left");
        assert_eq!(ssl_mode(&mut cx), before, "Left picks it back");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// The read-only toggle is a ring stop: Enter flips it, the saved profile
    /// carries it, and editing a read-only profile shows it switched on.
    #[::core::prelude::v1::test]
    fn the_form_ring_toggles_read_only_and_the_profile_keeps_it() {
        let mut cx = TestAppContext::single();
        use super::super::MainExtraStop;

        let window = open_postgres_form(&mut cx);

        let read_only_stop = window
            .update(&mut cx, |manager, _, _| {
                manager
                    .main_extra_stops()
                    .iter()
                    .position(|stop| matches!(stop, MainExtraStop::ReadOnly))
                    .map(|index| FormFocus::MainExtra(index as u8))
            })
            .expect("window is open")
            .expect("every driver offers the read-only toggle");

        window
            .update(&mut cx, |manager, _, _| manager.form_focus = read_only_stop)
            .expect("window is open");
        cx.simulate_keystrokes(window.into(), "enter");

        let profile = window
            .update(&mut cx, |manager, _, cx| manager.build_profile(cx))
            .expect("window is open")
            .expect("the form builds a profile");
        assert!(profile.read_only_flag, "Enter switches read-only on");

        let app_state = window
            .update(&mut cx, |manager, _, _| manager.app_state.clone())
            .expect("window is open");
        let edit_window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| {
                        ConnectionManagerWindow::new_for_edit(app_state, &profile, window, cx)
                    })
                })
            })
            .expect("connection manager edit window opens");
        let restored = edit_window
            .update(&mut cx, |manager, _, cx| {
                manager
                    .build_profile(cx)
                    .map(|profile| profile.read_only_flag)
            })
            .expect("edit window is open");
        assert_eq!(restored, Some(true), "editing keeps the profile read-only");

        cx.simulate_keystrokes(window.into(), "enter");
        let toggled_back = window
            .update(&mut cx, |manager, _, cx| {
                manager
                    .build_profile(cx)
                    .map(|profile| profile.read_only_flag)
            })
            .expect("window is open");
        assert_eq!(toggled_back, Some(false), "Enter switches read-only off");

        edit_window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// A driver field without a ring stop of its own, such as an auth
    /// profile picker, is an extra stop: Enter opens it and
    /// the dropdown keys drive it.
    #[::core::prelude::v1::test]
    fn the_form_ring_reaches_the_auth_profile_picker() {
        let mut cx = TestAppContext::single();
        use super::super::MainExtraStop;
        use datatab_core::FormFieldKind;

        let window = open_postgres_form(&mut cx);

        let picker_stop = window
            .update(&mut cx, |manager, window, cx| {
                let inner = manager
                    .form
                    .selected_driver
                    .clone()
                    .expect("PostgreSQL driver selected");
                let driver = super::super::keyboard_coverage_tests::FormFieldsTestDriver::with_auth_profile_ref(inner);
                manager.select_driver_with("postgres", Some(Arc::new(driver)), window, cx);
                manager.edit_state = EditState::Navigating;
                window.focus(&manager.focus_handle, cx);

                manager
                    .main_extra_stops()
                    .iter()
                    .position(|stop| {
                        matches!(
                            stop,
                            MainExtraStop::DriverField(field)
                                if matches!(field.kind, FormFieldKind::AuthProfileRef { .. })
                        )
                    })
                    .map(|index| FormFocus::MainExtra(index as u8))
            })
            .expect("window is open")
            .expect("the test driver form has an auth profile picker");

        window
            .update(&mut cx, |manager, _, _| manager.form_focus = picker_stop)
            .expect("window is open");
        cx.simulate_keystrokes(window.into(), "enter");

        let open = window
            .update(&mut cx, |manager, _, cx| {
                manager
                    .auth_profile
                    .auth_profile_dropdown
                    .read(cx)
                    .is_open()
            })
            .expect("window is open");
        assert!(open, "Enter opens the auth profile picker");

        cx.simulate_keystrokes(window.into(), "escape");
        let open = window
            .update(&mut cx, |manager, _, cx| {
                manager
                    .auth_profile
                    .auth_profile_dropdown
                    .read(cx)
                    .is_open()
            })
            .expect("window is open");
        assert!(!open, "Escape closes it");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// After a failed test, the banner's Copy button sits between Test
    /// connection and Save: Right reaches it and Enter copies the error.
    #[::core::prelude::v1::test]
    fn the_failed_test_copy_button_is_on_the_test_row() {
        let mut cx = TestAppContext::single();
        use super::super::TestStatus;

        let window = open_postgres_form(&mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                manager.test_status = TestStatus::Failed;
                manager.test_error = Some("connection refused".to_string());
                manager.form_focus = FormFocus::TestConnection;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("window is open");

        cx.simulate_keystrokes(window.into(), "l");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::CopyTestError);

        cx.simulate_keystrokes(window.into(), "enter");
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("connection refused")
        );

        cx.simulate_keystrokes(window.into(), "l");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::Save);

        cx.simulate_keystrokes(window.into(), "h h");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::TestConnection);

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// The Settings tab ring stops on each phase's hook dropdown before its
    /// extra hooks input; Enter hands the dropdown the keyboard and Escape
    /// gives it back to the form.
    #[::core::prelude::v1::test]
    fn the_settings_ring_opens_the_hook_dropdowns() {
        let mut cx = TestAppContext::single();
        use super::super::ActiveTab;

        let window = open_postgres_form(&mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                manager.active_tab = ActiveTab::Settings;
                manager.form_focus = FormFocus::SettingsRequiresPreview;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("window is open");
        cx.run_until_parked();

        cx.simulate_keystrokes(window.into(), "j");
        assert_eq!(
            form_state(window, &mut cx).0,
            FormFocus::SettingsPreConnectHook
        );

        cx.simulate_keystrokes(window.into(), "enter");
        let (focused, open) = window
            .update(&mut cx, |manager, window, cx| {
                let dropdown = manager.settings_tab.conn_pre_hook_dropdown.read(cx);
                (dropdown.is_focused(window), dropdown.is_open())
            })
            .expect("window is open");
        assert!(focused && open, "Enter opens the pre-connect hook dropdown");

        cx.simulate_keystrokes(window.into(), "escape");
        let root_focused = window
            .update(&mut cx, |manager, window, _| {
                manager.focus_handle.is_focused(window)
            })
            .expect("window is open");
        assert!(root_focused, "Escape gives the keyboard back to the form");
        assert_eq!(
            form_state(window, &mut cx).0,
            FormFocus::SettingsPreConnectHook
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// The MCP tab has a ring: the MCP switch, the client filter, each
    /// client, the selected client's access switch and its role picker.
    #[cfg(feature = "mcp")]
    #[::core::prelude::v1::test]
    fn the_mcp_tab_is_driven_by_the_form_ring() {
        let mut cx = TestAppContext::single();
        use super::super::ActiveTab;
        use datatab_mcp::TrustedClientDto;

        let window = open_postgres_form_with(&mut cx, |state| {
            state
                .upsert_mcp_trusted_client(TrustedClientDto {
                    id: "agent-a".to_string(),
                    name: "Agent A".to_string(),
                    issuer: None,
                    active: true,
                })
                .expect("the client is stored");
        });

        window
            .update(&mut cx, |manager, window, cx| {
                manager.active_tab = ActiveTab::Mcp;
                manager.form_focus = FormFocus::Name;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("window is open");
        cx.run_until_parked();

        let enabled = |cx: &mut TestAppContext| {
            window
                .update(cx, |manager, _, _| manager.mcp_tab.conn_mcp_enabled)
                .expect("window is open")
        };
        let before = enabled(&mut cx);

        cx.simulate_keystrokes(window.into(), "j");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::McpEnabled);
        cx.simulate_keystrokes(window.into(), "enter");
        assert_ne!(enabled(&mut cx), before, "Enter toggles MCP access");

        cx.simulate_keystrokes(window.into(), "j j");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::McpClient(0));
        cx.simulate_keystrokes(window.into(), "enter");
        assert_eq!(
            window
                .update(&mut cx, |manager, _, _| manager
                    .mcp_tab
                    .selected_actor_id
                    .clone())
                .expect("window is open")
                .as_deref(),
            Some("agent-a"),
            "Enter selects the client"
        );

        cx.simulate_keystrokes(window.into(), "j enter");
        let allowed = window
            .update(&mut cx, |manager, _, _| {
                manager
                    .mcp_tab
                    .bindings
                    .iter()
                    .any(|binding| binding.actor_id == "agent-a")
            })
            .expect("window is open");
        assert!(allowed, "Enter on the access switch allows the client");

        cx.simulate_keystrokes(window.into(), "j enter");
        assert_eq!(form_state(window, &mut cx).0, FormFocus::McpRole);
        let role_focused = window
            .update(&mut cx, |manager, window, cx| {
                manager
                    .mcp_tab
                    .conn_mcp_role_dropdown
                    .read(cx)
                    .is_focused(window)
            })
            .expect("window is open");
        assert!(role_focused, "Enter hands the role picker the keyboard");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    /// Page Down moves an open dropdown's highlight a page at a time.
    #[::core::prelude::v1::test]
    fn page_down_moves_an_open_dropdown() {
        let mut cx = TestAppContext::single();
        use super::super::ActiveTab;

        let window = open_postgres_form(&mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                manager.active_tab = ActiveTab::Access;
                manager.form_focus = FormFocus::AccessMethod;
                window.focus(&manager.focus_handle, cx);
            })
            .expect("window is open");
        cx.run_until_parked();

        let access_mode = |cx: &mut TestAppContext| {
            window
                .update(cx, |manager, _, _| manager.access.access_tab_mode)
                .expect("window is open")
        };
        let initial = access_mode(&mut cx);

        cx.simulate_keystrokes(window.into(), "enter pagedown enter");
        assert_ne!(
            access_mode(&mut cx),
            initial,
            "Page Down moved the highlight to another access method"
        );

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .log_err();
    }

    #[::core::prelude::v1::test]
    fn new_profile_password_save_success_persists_and_closes_the_real_window() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let app_state = test_app_state(
            &mut cx,
            SecretStoreFixture::new(PasswordSaveOutcome::Success),
        );
        let window = open_new_profile_window(app_state.clone(), &mut cx);

        window
            .update(&mut cx, |manager, window, cx| {
                manager.save_profile(window, cx)
            })
            .expect("successful save runs");
        cx.run_until_parked();

        assert!(
            window.root(&mut cx).is_err(),
            "a successful save closes the form window"
        );
        let profile = cx.update(|cx| {
            app_state
                .read(cx)
                .profiles()
                .first()
                .expect("new profile is persisted")
                .clone()
        });
        assert_eq!(profile.name, "new profile");
        assert_eq!(
            cx.update(|cx| {
                app_state
                    .read(cx)
                    .get_password(&profile)
                    .expect("keyring read succeeds")
                    .expect("primary password is persisted")
                    .expose_secret()
                    .to_string()
            }),
            "primary password"
        );
        assert_eq!(
            cx.update(|cx| host.read(cx).toast_count()),
            0,
            "success reports no error toast"
        );

        if window.root(&mut cx).is_ok() {
            window
                .update(&mut cx, |_, window, _| window.remove_window())
                .expect("close the test window");
        }
    }

    /// Keeps the latest rendered accessibility frame of the window it observes.
    #[derive(Default)]
    struct FrameCapture(Mutex<Option<gpui::AccessibilityFrame>>);

    impl gpui::FrameObserver for FrameCapture {
        fn accessibility_updated(&self, frame: &gpui::AccessibilityFrame) {
            *self.0.lock().expect("frame capture lock") = Some(frame.clone());
        }
    }

    /// Opens a connection manager window on the Postgres form with `password`
    /// typed into the password input, and returns the window with its latest
    /// rendered accessibility frame.
    fn render_postgres_form(
        password: &str,
        cx: &mut TestAppContext,
    ) -> (
        WindowHandle<ConnectionManagerWindow>,
        gpui::AccessibilityFrame,
    ) {
        init_form_test_runtime(cx);
        let app_state = test_app_state(cx, SecretStoreFixture::new(PasswordSaveOutcome::Success));

        let capture = Arc::new(FrameCapture::default());
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| ConnectionManagerWindow::new(app_state, window, cx))
                })
            })
            .expect("connection manager window opens");
        window
            .update(cx, |manager, window, cx| {
                manager.select_driver("postgres", window, cx);
                manager
                    .form
                    .input_password
                    .update(cx, |input, cx| input.set_value(password, window, cx));
                window.observe_frames(&capture);
                window.refresh();
            })
            .expect("postgres form initializes");
        cx.run_until_parked();

        let frame = capture
            .0
            .lock()
            .expect("frame capture lock")
            .clone()
            .expect("the window rendered a frame");

        (window, frame)
    }

    #[::core::prelude::v1::test]
    fn form_inputs_expose_stable_ids_and_their_field_labels() {
        let mut cx = TestAppContext::single();
        const PASSWORD: &str = "cm-automation-secret";

        let (window, frame) = render_postgres_form(PASSWORD, &mut cx);

        let text_inputs: HashMap<String, Option<String>> = frame
            .nodes()
            .filter_map(|(_, node)| {
                let accessible = frame.accessibility_node(node)?;
                matches!(
                    accessible.role(),
                    gpui::Role::TextInput | gpui::Role::PasswordInput
                )
                .then(|| {
                    (
                        node.id().to_owned(),
                        accessible.label().map(ToOwned::to_owned),
                    )
                })
            })
            .collect();

        let expected = [
            (
                "cm-field-name",
                datatab_i18n::t!("connection_manager.field.name"),
            ),
            ("cm-field-host", "Host".to_string()),
            ("cm-field-port", "Port".to_string()),
            ("cm-field-user", "User".to_string()),
        ];
        for (id, label) in expected {
            assert_eq!(
                text_inputs.get(id),
                Some(&Some(label)),
                "input {id} in {text_inputs:?}"
            );
        }

        let password_label = text_inputs
            .get("cm-field-password")
            .unwrap_or_else(|| panic!("password input in {text_inputs:?}"));
        assert!(
            password_label
                .as_deref()
                .is_some_and(|label| !label.is_empty()),
            "password input has no name: {text_inputs:?}"
        );
        assert!(
            text_inputs.keys().all(|id| id.starts_with("cm-")),
            "an input kept its per-run default id: {text_inputs:?}"
        );

        let password_exposed = frame.nodes().any(|(_, node)| {
            node.content_text().contains(PASSWORD)
                || frame.accessibility_node(node).is_some_and(|accessible| {
                    [accessible.value(), accessible.label()]
                        .into_iter()
                        .flatten()
                        .any(|text| text.contains(PASSWORD))
                })
        });
        assert!(!password_exposed, "the password value reached the frame");

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("connection manager window closes");
    }

    /// The form tabs are exposed as tabs inside one tab list, and only the
    /// active tab reports itself selected.
    #[::core::prelude::v1::test]
    fn form_tabs_are_exposed_as_selectable_tabs_in_a_tab_list() {
        let mut cx = TestAppContext::single();
        let (window, frame) = render_postgres_form("", &mut cx);

        let node = |id: &str| {
            frame
                .nodes()
                .find(|(_, node)| node.id() == id)
                .and_then(|(_, node)| frame.accessibility_node(node))
                .unwrap_or_else(|| panic!("no accessible node {id}"))
        };

        assert_eq!(node("cm-tab-list").role(), gpui::Role::TabList);

        let tabs = [
            ("tab-main", true),
            ("tab-access", false),
            ("tab-settings", false),
            ("tab-mcp", false),
        ];
        for (id, selected) in tabs {
            let tab = node(id);
            assert_eq!(tab.role(), gpui::Role::Tab, "tab {id}");
            assert_eq!(tab.is_selected(), Some(selected), "tab {id}");
        }

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("connection manager window closes");
    }

    /// Stores one Command hook definition per name directly in storage and
    /// returns an app state that loads them, with each name mapped to the id
    /// the profile bindings must reference.
    fn app_state_with_hooks(
        names: &[&str],
        cx: &mut TestAppContext,
    ) -> (Entity<AppStateEntity>, HashMap<String, String>) {
        use datatab_storage::repositories::hook_definitions::HookDefinitionDto;

        let runtime = StorageRuntime::in_memory().expect("test storage runtime");
        let mut ids = HashMap::new();

        for name in names {
            let id = uuid::Uuid::new_v4();
            let mut definition =
                HookDefinitionDto::new(id, (*name).to_string(), "Command".to_string());
            definition.kind_json =
                Some(r#"{"kind":"command","command":"echo hi","args":[]}"#.to_string());

            runtime
                .hook_definitions()
                .upsert(&definition)
                .expect("seed hook definition");
            ids.insert((*name).to_string(), id.to_string());
        }

        let app_state = cx.update(|cx| {
            cx.new(|_| AppStateEntity::new_with_storage_runtime(runtime).expect("test app state"))
        });
        app_state.update(cx, |state, _| {
            *state
                .secret_store()
                .write()
                .expect("test secret store lock poisoned") =
                Box::new(SecretStoreFixture::new(PasswordSaveOutcome::Success));
        });

        (app_state, ids)
    }

    fn hook_ids(ids: &HashMap<String, String>, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .map(|name| ids.get(*name).expect("seeded hook id").clone())
            .collect()
    }

    /// A profile whose phases bind a primary hook plus extras, so the edit
    /// form splits each phase into its dropdown and its extra input.
    fn profile_with_hook_bindings(ids: &HashMap<String, String>) -> ConnectionProfile {
        let mut profile = sqlite_profile("hooked profile");
        profile.hook_bindings = Some(datatab_core::ConnectionHookBindings {
            pre_connect: hook_ids(ids, &["alpha", "beta", "gamma"]),
            post_connect: Vec::new(),
            pre_disconnect: hook_ids(ids, &["beta"]),
            post_disconnect: hook_ids(ids, &["alpha", "gamma"]),
        });
        profile
    }

    const HOOK_EXTRA_FIELDS: [(&str, &str); 4] = [
        (
            "cm-setting-pre_connect_hook_extra",
            "hooks.phase.extra_pre_connect",
        ),
        (
            "cm-setting-post_connect_hook_extra",
            "hooks.phase.extra_post_connect",
        ),
        (
            "cm-setting-pre_disconnect_hook_extra",
            "hooks.phase.extra_pre_disconnect",
        ),
        (
            "cm-setting-post_disconnect_hook_extra",
            "hooks.phase.extra_post_disconnect",
        ),
    ];

    #[::core::prelude::v1::test]
    fn hook_extra_inputs_render_with_ids_labels_and_loaded_values() {
        let mut cx = TestAppContext::single();
        init_form_test_runtime(&mut cx);
        let (app_state, ids) = app_state_with_hooks(&["alpha", "beta", "gamma"], &mut cx);
        let profile = profile_with_hook_bindings(&ids);

        let capture = Arc::new(FrameCapture::default());
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| {
                        ConnectionManagerWindow::new_for_edit(app_state, &profile, window, cx)
                    })
                })
            })
            .expect("connection manager edit window opens");
        window
            .update(&mut cx, |manager, window, _cx| {
                manager.active_tab = super::super::ActiveTab::Settings;
                window.observe_frames(&capture);
                window.refresh();
            })
            .expect("settings tab opens");
        cx.run_until_parked();

        let frame = capture
            .0
            .lock()
            .expect("frame capture lock")
            .clone()
            .expect("the window rendered a frame");

        let text_inputs: HashMap<String, (Option<String>, Option<String>)> = frame
            .nodes()
            .filter_map(|(_, node)| {
                let accessible = frame.accessibility_node(node)?;
                (accessible.role() == gpui::Role::TextInput).then(|| {
                    (
                        node.id().to_owned(),
                        (
                            accessible.label().map(ToOwned::to_owned),
                            accessible.value().map(ToOwned::to_owned),
                        ),
                    )
                })
            })
            .collect();

        let expected_values = ["beta, gamma", "", "", "gamma"];
        for ((id, label_key), value) in HOOK_EXTRA_FIELDS.into_iter().zip(expected_values) {
            let (label, rendered_value) = text_inputs
                .get(id)
                .unwrap_or_else(|| panic!("input {id} is not rendered: {text_inputs:?}"));

            assert_eq!(
                label.as_deref(),
                Some(datatab_i18n::t!(label_key).as_str()),
                "input {id} is named after its visible label"
            );
            assert_eq!(
                rendered_value.as_deref().unwrap_or_default(),
                value,
                "input {id} shows the loaded extra hooks"
            );
        }

        window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("connection manager window closes");
    }

    #[::core::prelude::v1::test]
    fn hook_extra_inputs_round_trip_loaded_and_typed_values_on_save() {
        let mut cx = TestAppContext::single();
        let host = init_form_test_runtime(&mut cx);
        let (app_state, ids) = app_state_with_hooks(&["alpha", "beta", "gamma"], &mut cx);
        let profile = profile_with_hook_bindings(&ids);
        app_state.update(&mut cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });

        let window = open_edit_profile_window(app_state.clone(), profile, &mut cx);
        cx.run_until_parked();
        window
            .update(&mut cx, |manager, window, cx| {
                manager
                    .settings_tab
                    .conn_post_hook_extra_input
                    .update(cx, |input, cx| input.set_value("gamma, alpha", window, cx));
                manager
                    .settings_tab
                    .conn_pre_disconnect_hook_extra_input
                    .update(cx, |input, cx| {
                        input.set_value(ids["gamma"].clone(), window, cx)
                    });
                manager.save_profile(window, cx)
            })
            .expect("save runs");
        cx.run_until_parked();

        assert_eq!(
            cx.update(|cx| host.read(cx).toast_count()),
            0,
            "the save reports no error"
        );
        assert!(
            window.root(&mut cx).is_err(),
            "a successful save closes the form window"
        );

        let bindings = cx.update(|cx| {
            app_state
                .read(cx)
                .profiles()
                .first()
                .expect("the edited profile is persisted")
                .hook_bindings
                .clone()
                .expect("hook bindings are saved")
        });

        assert_eq!(
            bindings.pre_connect,
            hook_ids(&ids, &["alpha", "beta", "gamma"]),
            "loaded extras survive an untouched save"
        );
        assert_eq!(
            bindings.post_connect,
            hook_ids(&ids, &["gamma", "alpha"]),
            "typed hook names resolve to ids in the typed order"
        );
        assert_eq!(
            bindings.pre_disconnect,
            hook_ids(&ids, &["beta", "gamma"]),
            "a typed hook id follows the dropdown hook"
        );
        assert_eq!(
            bindings.post_disconnect,
            hook_ids(&ids, &["alpha", "gamma"])
        );
    }
}

/// SQL Server SSL mode tests. A separate module with explicit imports:
/// combining `use super::*` with `#[gpui::test]` sends the gpui_macros
/// expansion into unbounded recursion (see `ssh_config_host_tests`).
#[cfg(test)]
mod sqlserver_ssl_tests {
    use crate::connection_manager::ConnectionManagerWindow;
    use datatab_core::{ConnectionProfile, DbConfig};
    use datatab_storage::bootstrap::StorageRuntime;
    use datatab_ui_base::AppStateEntity;
    use datatab_ui_base::toast::{ToastGlobal, ToastHost};
    use gpui::{AppContext as _, Entity, TestAppContext, WindowHandle, WindowOptions};

    fn init_test_runtime(cx: &mut TestAppContext) -> Entity<ToastHost> {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        cx.update(|cx| {
            let host = cx.new(|_| ToastHost::new());
            cx.set_global(ToastGlobal { host: host.clone() });
            host
        })
    }

    fn test_app_state(cx: &mut TestAppContext) -> Entity<AppStateEntity> {
        cx.update(|cx| {
            cx.new(|_| {
                AppStateEntity::new_with_storage_runtime(
                    StorageRuntime::in_memory().expect("test storage runtime"),
                )
                .expect("test app state")
            })
        })
    }

    fn open_sqlserver_form_window(
        app_state: Entity<AppStateEntity>,
        cx: &mut TestAppContext,
    ) -> WindowHandle<ConnectionManagerWindow> {
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| ConnectionManagerWindow::new(app_state, window, cx))
                })
            })
            .expect("connection manager window opens");
        window
            .update(cx, |manager, window, cx| {
                manager.select_driver("mssql", window, cx);
                for (field, value) in [("host", "db.corp"), ("port", "1433"), ("user", "sa")] {
                    manager
                        .form
                        .driver_inputs
                        .get(field)
                        .unwrap_or_else(|| panic!("mssql form has a `{field}` input"))
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
            })
            .expect("mssql form initializes");
        window
    }

    fn build_sqlserver_config(
        window: &WindowHandle<ConnectionManagerWindow>,
        ssl_mode: &str,
        ca_path: Option<&str>,
        cx: &mut TestAppContext,
    ) -> DbConfig {
        window
            .update(cx, |manager, window, cx| {
                manager.form.selected_ssl_mode = ssl_mode.to_string();
                if let Some(ca_path) = ca_path {
                    manager.form.ssl_ca_cert_input.update(cx, |input, cx| {
                        input.set_value(ca_path, window, cx);
                    });
                }
                manager
                    .build_config(cx)
                    .expect("the SQL Server config builds")
            })
            .expect("read the built config")
    }

    /// Selecting `required` for SQL Server with a CA path must save a config
    /// that validates the certificate chain (S2): the mode is carried and
    /// `trust_server_certificate` is cleared for `required`, because
    /// `collect_form_values` does not carry the selected mode.
    #[gpui::test]
    fn sqlserver_required_mode_builds_a_chain_validating_config_with_the_ca_path(
        cx: &mut TestAppContext,
    ) {
        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let window = open_sqlserver_form_window(app_state, cx);

        let config = build_sqlserver_config(&window, "required", Some("/etc/ssl/corp-ca.pem"), cx);

        match config {
            DbConfig::SqlServer {
                ssl_mode,
                trust_server_certificate,
                ssl_root_cert_path,
                ..
            } => {
                assert_eq!(ssl_mode.as_deref(), Some("required"));
                assert!(
                    !trust_server_certificate,
                    "`required` must validate the chain, not call trust_cert()"
                );
                assert_eq!(ssl_root_cert_path.as_deref(), Some("/etc/ssl/corp-ca.pem"));
            }
            _ => panic!("expected SqlServer config"),
        }
    }

    /// The reverse of the `required` case: `on` keeps the driver's
    /// trust-cert behavior (encrypted, self-signed certs accepted).
    #[gpui::test]
    fn sqlserver_on_mode_builds_a_trusting_config(cx: &mut TestAppContext) {
        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let window = open_sqlserver_form_window(app_state, cx);

        let config = build_sqlserver_config(&window, "on", None, cx);

        match config {
            DbConfig::SqlServer {
                ssl_mode,
                trust_server_certificate,
                ssl_root_cert_path,
                ..
            } => {
                assert_eq!(ssl_mode.as_deref(), Some("on"));
                assert!(
                    trust_server_certificate,
                    "`on` accepts self-signed certs via trust_cert()"
                );
                assert_eq!(ssl_root_cert_path, None);
            }
            _ => panic!("expected SqlServer config"),
        }
    }

    /// Editing a saved SQL Server profile must restore its saved mode, not
    /// silently reset it to the driver's first declared mode (`off`).
    #[gpui::test]
    fn editing_a_saved_sqlserver_profile_restores_its_ssl_mode(cx: &mut TestAppContext) {
        let mut config = DbConfig::default_sqlserver();
        if let DbConfig::SqlServer { ssl_mode, .. } = &mut config {
            *ssl_mode = Some("required".to_string());
        }
        let profile = ConnectionProfile::new("mssql required", config);

        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| {
                        ConnectionManagerWindow::new_for_edit(app_state, &profile, window, cx)
                    })
                })
            })
            .expect("connection manager edit window opens");

        let selected_mode = window
            .update(cx, |manager, _window, _cx| {
                manager.form.selected_ssl_mode.clone()
            })
            .expect("read the rehydrated SSL mode");
        assert_eq!(selected_mode, "required");

        let rebuilt = window
            .update(cx, |manager, _window, cx| {
                manager.build_config(cx).map(|config| match config {
                    DbConfig::SqlServer {
                        ssl_mode,
                        trust_server_certificate,
                        ssl_root_cert_path,
                        ..
                    } => (ssl_mode, trust_server_certificate, ssl_root_cert_path),
                    _ => panic!("expected SqlServer config"),
                })
            })
            .expect("rebuild the edited config");
        let (ssl_mode, trust_server_certificate, ssl_root_cert_path) =
            rebuilt.expect("the edited profile rebuilds its config");
        assert_eq!(ssl_mode.as_deref(), Some("required"));
        assert!(!trust_server_certificate);
        assert_eq!(ssl_root_cert_path, None);
    }
}
/// Tests of the SSH config host picker on the Access tab (#837, T5). A
/// separate module with explicit imports: combining `use super::*` with
/// `#[gpui::test]` sends the gpui_macros expansion into unbounded recursion
/// (see `keyboard_coverage_tests` in `connection_manager/mod.rs`).
#[cfg(test)]
mod ssh_config_host_tests {
    use crate::connection_manager::{AccessTabMode, ConnectionManagerWindow};
    use datatab_storage::bootstrap::StorageRuntime;
    use datatab_ui_base::AppStateEntity;
    use datatab_ui_base::toast::{ToastGlobal, ToastHost};
    use gpui::{AppContext as _, Entity, TestAppContext, WindowHandle, WindowOptions};

    fn init_test_runtime(cx: &mut TestAppContext) -> Entity<ToastHost> {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        cx.update(|cx| {
            let host = cx.new(|_| ToastHost::new());
            cx.set_global(ToastGlobal { host: host.clone() });
            host
        })
    }

    fn test_app_state(cx: &mut TestAppContext) -> Entity<AppStateEntity> {
        cx.update(|cx| {
            cx.new(|_| {
                AppStateEntity::new_with_storage_runtime(
                    StorageRuntime::in_memory().expect("test storage runtime"),
                )
                .expect("test app state")
            })
        })
    }

    // --- SSH config host picker on the Access tab (#837, T5) ---

    /// A throwaway fixture directory with a writable `config` file. Never
    /// the real home directory (S7).
    struct SshConfigFixture(std::path::PathBuf);

    impl SshConfigFixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "datatab-cm-ssh-config-{name}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).expect("create fixture dir");
            Self(dir)
        }

        fn write_config(&self, text: &str) {
            std::fs::write(self.0.join("config"), text).expect("write fixture config");
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }

        /// A home directory inside the fixture, so `~` expansion never
        /// reaches the real home (S7).
        fn home(&self) -> std::path::PathBuf {
            self.0.join("home")
        }
    }

    impl Drop for SshConfigFixture {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("fixture cleanup failed: {error}");
            }
        }
    }

    fn ssh_config_fixture(name: &str) -> SshConfigFixture {
        let fixture = SshConfigFixture::new(name);
        fixture.write_config(
            "Host alpha\n  HostName alpha.example.com\n  User deploy\n  Port 2222\n\nHost beta\n  HostName 10.0.0.5\n\nHost *\n  Compression yes\n",
        );
        fixture
    }

    fn open_ssh_form_window(
        app_state: Entity<AppStateEntity>,
        cx: &mut TestAppContext,
    ) -> WindowHandle<ConnectionManagerWindow> {
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| ConnectionManagerWindow::new(app_state, window, cx))
                })
            })
            .expect("connection manager window opens");
        window
            .update(cx, |manager, window, cx| {
                manager.select_driver("postgres", window, cx);
                manager.access.ssh_enabled = true;
                manager.access.access_tab_mode = AccessTabMode::Ssh;
            })
            .expect("ssh inline form initializes");
        window
    }

    fn inject_fixture_hosts(
        window: &WindowHandle<ConnectionManagerWindow>,
        fixture: &SshConfigFixture,
        cx: &mut TestAppContext,
    ) {
        window
            .update(cx, |manager, _window, cx| {
                let snapshot =
                    crate::ssh_host_picker::load_ssh_hosts(fixture.path(), &fixture.home());
                manager
                    .access
                    .ssh_host_picker
                    .update(cx, |picker, cx| picker.apply_snapshot(snapshot, cx));
            })
            .expect("fixture hosts load into the picker");
    }

    /// The Access tab picker lists the fixture hosts, and selecting one
    /// makes the built SSH config reference the alias with empty host, empty
    /// user and port 22 (A7).
    #[gpui::test]
    fn the_access_tab_picker_lists_fixture_hosts_and_the_alias_reaches_the_built_config(
        cx: &mut TestAppContext,
    ) {
        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let fixture = ssh_config_fixture("lists-and-builds");
        let window = open_ssh_form_window(app_state, cx);

        inject_fixture_hosts(&window, &fixture, cx);

        let aliases = window
            .update(cx, |manager, _window, cx| {
                manager
                    .access
                    .ssh_host_picker
                    .read(cx)
                    .snapshot()
                    .hosts
                    .iter()
                    .map(|host| host.alias.clone())
                    .collect::<Vec<_>>()
            })
            .expect("read the picker snapshot");
        assert_eq!(
            aliases,
            vec!["alpha".to_string(), "beta".to_string()],
            "the picker lists the fixture hosts in file order; `Host *` is not pickable (A5)"
        );

        window
            .update(cx, |manager, window, cx| {
                manager.apply_ssh_config_host_index(1, window, cx)
            })
            .expect("select the first fixture host");

        let config = window
            .update(cx, |manager, _window, cx| {
                manager
                    .build_ssh_config(cx)
                    .expect("the inline SSH config builds")
            })
            .expect("read the built config");
        assert_eq!(config.ssh_config_host.as_deref(), Some("alpha"));
        assert_eq!(config.host, "", "the alias is the only target (A7)");
        assert_eq!(config.user, "");
        assert_eq!(config.port, 22);
    }

    /// With an alias active the empty host and user are correct, so the
    /// required-field validation must not fire; without the alias the same
    /// empty form still reports both.
    #[gpui::test]
    fn with_an_alias_validation_does_not_demand_host_or_user(cx: &mut TestAppContext) {
        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let fixture = ssh_config_fixture("validation");
        let window = open_ssh_form_window(app_state, cx);

        let (ok, host_error, user_error) = window
            .update(cx, |manager, _window, cx| {
                let ok = manager.validate_form(false, cx);
                let host_error = datatab_i18n::t!("form.validation.ssh_host_required");
                let user_error = datatab_i18n::t!("form.validation.ssh_user_required");
                (
                    ok,
                    manager.validation_errors.contains(&host_error),
                    manager.validation_errors.contains(&user_error),
                )
            })
            .expect("validate the manual form");
        assert!(!ok, "an empty manual SSH form is invalid");
        assert!(host_error, "the missing host is reported");
        assert!(user_error, "the missing user is reported");

        inject_fixture_hosts(&window, &fixture, cx);
        window
            .update(cx, |manager, window, cx| {
                manager.apply_ssh_config_host_index(1, window, cx)
            })
            .expect("select the first fixture host");

        let (ok, host_error, user_error) = window
            .update(cx, |manager, _window, cx| {
                let ok = manager.validate_form(false, cx);
                let host_error = datatab_i18n::t!("form.validation.ssh_host_required");
                let user_error = datatab_i18n::t!("form.validation.ssh_user_required");
                (
                    ok,
                    manager.validation_errors.contains(&host_error),
                    manager.validation_errors.contains(&user_error),
                )
            })
            .expect("validate the aliased form");
        assert!(ok, "an aliased form needs no manual host or user");
        assert!(!host_error, "the alias supplies the host");
        assert!(!user_error, "the alias supplies the user");
    }

    /// Clearing the alias (back to the manual entry) restores exactly the
    /// manual target that was typed before the alias was picked.
    #[gpui::test]
    fn clearing_the_alias_restores_the_manual_target_in_the_built_config(cx: &mut TestAppContext) {
        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let fixture = ssh_config_fixture("clearing");
        let window = open_ssh_form_window(app_state, cx);

        window
            .update(cx, |manager, window, cx| {
                manager.access.input_ssh_host.update(cx, |state, cx| {
                    state.set_value("manual.example.com", window, cx)
                });
                manager
                    .access
                    .input_ssh_port
                    .update(cx, |state, cx| state.set_value("2200", window, cx));
                manager
                    .access
                    .input_ssh_user
                    .update(cx, |state, cx| state.set_value("manualuser", window, cx));
            })
            .expect("type the manual target");

        inject_fixture_hosts(&window, &fixture, cx);
        window
            .update(cx, |manager, window, cx| {
                manager.apply_ssh_config_host_index(1, window, cx)
            })
            .expect("select the alias");

        let aliased = window
            .update(cx, |manager, _window, cx| {
                manager
                    .build_ssh_config(cx)
                    .expect("the aliased config builds")
            })
            .expect("read the built config");
        assert_eq!(aliased.ssh_config_host.as_deref(), Some("alpha"));
        assert_eq!(aliased.host, "");

        window
            .update(cx, |manager, window, cx| {
                manager.apply_ssh_config_host_index(0, window, cx)
            })
            .expect("clear back to the manual entry");

        let manual = window
            .update(cx, |manager, _window, cx| {
                manager
                    .build_ssh_config(cx)
                    .expect("the manual config builds")
            })
            .expect("read the built config");
        assert_eq!(manual.ssh_config_host, None);
        assert_eq!(manual.host, "manual.example.com");
        assert_eq!(manual.user, "manualuser");
        assert_eq!(manual.port, 2200);
    }

    /// "Save as tunnel" from an aliased inline config promotes the alias —
    /// not the resolved values — into the saved profile (S2).
    #[gpui::test]
    fn save_as_tunnel_from_an_aliased_inline_config_carries_the_alias(cx: &mut TestAppContext) {
        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let fixture = ssh_config_fixture("save-as-tunnel");
        let window = open_ssh_form_window(app_state.clone(), cx);
        inject_fixture_hosts(&window, &fixture, cx);

        window
            .update(cx, |manager, window, cx| {
                manager.apply_ssh_config_host_index(1, window, cx);
                manager.save_current_ssh_as_tunnel(cx);
            })
            .expect("save the aliased inline config as a tunnel");

        let saved = cx.update(|cx| {
            app_state
                .read(cx)
                .ssh_tunnels()
                .last()
                .cloned()
                .expect("the tunnel profile is saved")
        });
        assert_eq!(saved.config.ssh_config_host.as_deref(), Some("alpha"));
        assert_eq!(saved.config.host, "", "no resolved value is copied (S2)");
        assert_eq!(saved.config.user, "");
        assert_eq!(saved.config.port, 22);
        assert_eq!(saved.name, "alpha", "the profile is named after the alias");
    }

    /// The saved-tunnel selector keeps its previous behaviour: applying a
    /// manual tunnel fills the inline fields exactly and they validate, and
    /// clearing the selection resets the form (PRESERVE).
    #[gpui::test]
    fn applying_a_saved_manual_tunnel_still_fills_the_inline_fields_and_validates(
        cx: &mut TestAppContext,
    ) {
        use datatab_core::{SshAuthMethod, SshTunnelConfig, SshTunnelProfile};

        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let window = open_ssh_form_window(app_state, cx);

        let tunnel = SshTunnelProfile {
            id: uuid::Uuid::new_v4(),
            name: "manual tunnel".to_string(),
            config: SshTunnelConfig {
                host: "bastion.example.com".to_string(),
                port: 2222,
                user: "deploy".to_string(),
                auth_method: SshAuthMethod::PrivateKey { key_path: None },
                ssh_config_host: None,
            },
            save_secret: false,
        };

        window
            .update(cx, |manager, window, cx| {
                manager.apply_ssh_tunnel(&tunnel, None, window, cx);
            })
            .expect("apply the saved manual tunnel");

        let (host, port, user, ok) = window
            .update(cx, |manager, _window, cx| {
                let host = manager.access.input_ssh_host.read(cx).value().to_string();
                let port = manager.access.input_ssh_port.read(cx).value().to_string();
                let user = manager.access.input_ssh_user.read(cx).value().to_string();
                let ok = manager.validate_form(false, cx);
                (host, port, user, ok)
            })
            .expect("read the applied tunnel");
        assert_eq!(host, "bastion.example.com");
        assert_eq!(port, "2222");
        assert_eq!(user, "deploy");
        assert!(ok, "the applied manual tunnel validates");

        window
            .update(cx, |manager, window, cx| {
                manager.clear_ssh_tunnel_selection(window, cx);
            })
            .expect("clear the tunnel selection");

        let (host, port, user, alias) = window
            .update(cx, |manager, _window, cx| {
                let host = manager.access.input_ssh_host.read(cx).value().to_string();
                let port = manager.access.input_ssh_port.read(cx).value().to_string();
                let user = manager.access.input_ssh_user.read(cx).value().to_string();
                let alias = manager
                    .access
                    .ssh_host_picker
                    .read(cx)
                    .selected_alias()
                    .map(str::to_string);
                (host, port, user, alias)
            })
            .expect("read the cleared selection");
        assert_eq!(host, "");
        assert_eq!(port, "22");
        assert_eq!(user, "");
        assert_eq!(alias, None, "clearing leaves no stale alias");
    }

    /// Rehydration of a saved inline profile that references an alias: the
    /// picker shows the alias, the manual inputs stay as stored — empty —
    /// and building the config again reproduces the stored one exactly (A7,
    /// S2). The connection manager keeps no dirty flag, so "not changed"
    /// means the rebuilt config equals the stored one.
    #[gpui::test]
    fn a_freshly_opened_aliased_profile_rehydrates_to_the_stored_config(cx: &mut TestAppContext) {
        use datatab_core::{ConnectionProfile, DbConfig};

        let mut stored_tunnel = crate::ssh_shared::build_ssh_config_with_alias(
            "ignored",
            "2200",
            "ignored",
            crate::ssh_shared::SshAuthSelection::PrivateKey,
            "",
            Some("alpha"),
        );
        stored_tunnel.auth_method = datatab_core::SshAuthMethod::PrivateKey { key_path: None };

        let profile = ConnectionProfile::new(
            "aliased inline",
            DbConfig::Postgres {
                use_uri: false,
                uri: None,
                host: "db.internal".into(),
                port: 5432,
                user: "app".into(),
                database: "appdb".into(),
                ssl_mode: None,
                ssl_root_cert_path: None,
                ssl_client_cert_path: None,
                ssl_client_key_path: None,
                ssh_tunnel: Some(stored_tunnel.clone()),
                ssh_tunnel_profile_id: None,
            },
        );

        let _toast_host = init_test_runtime(cx);
        let app_state = test_app_state(cx);
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| {
                        ConnectionManagerWindow::new_for_edit(app_state, &profile, window, cx)
                    })
                })
            })
            .expect("connection manager edit window opens");

        let (alias, host, user, port, rebuilt) = window
            .update(cx, |manager, _window, cx| {
                let alias = manager.ssh_config_alias(cx);
                let host = manager.access.input_ssh_host.read(cx).value().to_string();
                let user = manager.access.input_ssh_user.read(cx).value().to_string();
                let port = manager.access.input_ssh_port.read(cx).value().to_string();
                let rebuilt = manager.build_ssh_config(cx);
                (alias, host, user, port, rebuilt)
            })
            .expect("read the rehydrated form");

        assert_eq!(
            alias.as_deref(),
            Some("alpha"),
            "the picker shows the alias"
        );
        assert_eq!(host, "", "the manual inputs stay as stored (A7)");
        assert_eq!(user, "");
        assert_eq!(port, "22");
        let rebuilt = rebuilt.expect("the rehydrated form rebuilds the config");
        assert_eq!(rebuilt.ssh_config_host, stored_tunnel.ssh_config_host);
        assert_eq!(rebuilt.host, stored_tunnel.host);
        assert_eq!(rebuilt.user, stored_tunnel.user);
        assert_eq!(rebuilt.port, stored_tunnel.port);
        assert_eq!(
            rebuilt.auth_method, stored_tunnel.auth_method,
            "the rebuilt config equals the stored one: nothing reads as changed"
        );
    }
}
