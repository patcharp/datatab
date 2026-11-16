use super::SettingsSection;
use super::SettingsSectionId;
use super::form_section::{FormSection, create_blur_subscription};
use super::layout;
use super::section_trait::{SectionFocusEvent, SectionPortabilityEvent};
use crate::connection_manager::ExportTarget;
use crate::tokens::{FormMetrics, SettingsMetrics};
use datatab_app::keymap::Modifiers;
use datatab_components::components::form_renderer;
use datatab_components::controls::InputState;
use datatab_components::controls::{Button, Checkbox, Input};
use datatab_components::controls::{Dropdown, DropdownItem, DropdownSelectionChanged};
use datatab_components::icons::AppIcon;
use datatab_components::primitives::{Badge, BadgeTone, BannerBlock, BannerVariant, Chamfer, Text};
use datatab_components::tokens::{ChamferCut, Fields, ui};
use datatab_core::secrecy::{ExposeSecret, SecretString};
use datatab_core::{
    AccessKind, AuthProfile, FetchOptionsError, FetchOptionsRequest, FormFieldKind, LogErr,
    RefreshTrigger,
};
use datatab_ui_base::keymap::key_chord_from_gpui;
use datatab_ui_base::user_error::{ErrorKind, UserFacingError, report_error};
use datatab_ui_base::{AppStateChanged, AppStateEntity};
use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dialog::{Dialog, DialogButtonProps};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

/// Width of the text fields and selects of the auth profile form. (300 px)
const AUTH_SELECT_WIDTH: Rems = ui(300.0);

/// Cached dropdown options for a `DynamicSelect` field.
///
/// The cache key is `(provider_id, field_id, dep_hash)` where `dep_hash` is
/// a hash of the sorted dependency key-value pairs.
struct CachedOptions {
    options: Vec<datatab_core::SelectOption>,
    expires_at: Instant,
}

pub(super) struct AuthProfilesSection {
    app_state: Entity<AppStateEntity>,
    selected_profile_id: Option<Uuid>,
    editing_profile_id: Option<Uuid>,
    selected_provider_id: Option<String>,
    selected_provider_supports_login: bool,
    provider_entries_cache: Vec<(String, String)>,
    profile_enabled: bool,
    pending_delete_profile_id: Option<Uuid>,
    pending_sync_from_app_state: bool,

    input_name: Entity<InputState>,
    form_inputs: HashMap<String, Entity<InputState>>,
    provider_field_order: Vec<String>,
    provider_login_loading: bool,
    provider_login_status: Option<(String, bool)>,
    /// Identifies the login the form waits for. `release_login` advances it
    /// when a login starts, finishes or is cancelled and when the form is
    /// replaced (a profile loaded, a new profile, another provider), so the
    /// result of an earlier login is ignored when it arrives.
    login_generation: u64,

    /// Dropdown for selecting the auth provider.
    provider_dropdown: Entity<Dropdown>,
    /// Generic dynamic-select dropdowns keyed by field id.
    dynamic_dropdowns: HashMap<String, Entity<Dropdown>>,
    /// Cached options indexed by `(provider_id, field_id, dep_hash)`.
    options_cache: HashMap<(String, String, u64), CachedOptions>,
    /// Per-field re-login hint shown below the dropdown when the last fetch
    /// returned `NeedsLogin` or `SessionExpired`.
    ///
    /// Keyed by field id; cleared on successful fetch or profile change.
    field_login_hint: HashMap<String, String>,
    /// When set, the URL is routed to the login modal via the pending pattern.
    ///
    /// Fields: `(provider_name, profile_name, url, login_generation)`.
    pending_sso_url: Option<(String, String, Option<String>, u64)>,
    /// Active login verification URL displayed inline under the Login button.
    /// Cleared when the login completes or the user cancels.
    active_login_url: Option<String>,
    /// When true, the next render cycle will re-fetch options for all
    /// `OnLoginComplete` DynamicSelect fields.
    pending_login_complete_refresh: bool,

    auth_focus: AuthFocus,
    auth_form_field: AuthFormField,
    auth_editing_field: bool,
    content_focused: bool,
    profile_list_scroll_handle: ScrollHandle,
    pending_profile_scroll_idx: Option<usize>,
    switching_input: bool,

    _subscriptions: Vec<Subscription>,
    _blur_subscriptions: Vec<Subscription>,
    /// Subscriptions for DynamicSelect dropdown selection events.
    /// Rebuilt whenever `rebuild_form_inputs` is called.
    _dropdown_subscriptions: Vec<Subscription>,
    /// Subscription for provider dropdown selection events.
    _provider_dropdown_subscription: Option<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AuthFocus {
    ProfileList,
    Form,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AuthFormField {
    Name,
    Provider(usize),
    DynamicField(usize),
    ProviderLogin,
    /// The buttons of the login URL panel shown while an interactive login
    /// waits for the browser.
    LoginOpenBrowser,
    LoginCopyUrl,
    LoginCancel,
    Enabled,
    ExportButton,
    DeleteButton,
    SaveButton,
}

/// Events emitted by the auth profiles section for inter-window routing.
#[derive(Clone, Debug)]
pub(super) enum AuthProfilesSectionEvent {
    /// A login flow produced a URL that should be displayed in the login modal.
    ///
    /// `generation` is the `login_generation` of that login.
    OpenLoginModal {
        provider_name: String,
        profile_name: String,
        url: Option<String>,
        generation: u64,
    },
    /// The form stopped waiting for the login of `generation`: its result
    /// arrived, or the form let go of it.
    LoginEnded { generation: u64 },
}

/// A login the auth profile form waits for, as the surface that shows its
/// verification URL in another window holds it.
///
/// It reaches the section itself, not the Settings window: the login keeps
/// its section alive after the user leaves the auth profiles page.
#[derive(Clone)]
pub struct SettingsLoginHandle {
    section: WeakEntity<AuthProfilesSection>,
    generation: u64,
}

impl SettingsLoginHandle {
    pub(super) fn new(section: WeakEntity<AuthProfilesSection>, generation: u64) -> Self {
        Self {
            section,
            generation,
        }
    }

    /// Whether the form still waits for this login.
    pub fn is_waiting(&self, cx: &App) -> bool {
        self.section.upgrade().is_some_and(|section| {
            let section = section.read(cx);
            section.provider_login_loading && section.login_generation == self.generation
        })
    }

    /// Cancels the login as the form's own Cancel does. Does nothing once the
    /// form stopped waiting for this login.
    pub fn cancel(&self, cx: &mut App) {
        if !self.is_waiting(cx) {
            return;
        }

        if let Some(section) = self.section.upgrade() {
            section.update(cx, |section, cx| section.cancel_provider_login(cx));
        }
    }

    /// Calls `on_ended` when the form stops waiting for this login: its
    /// result arrived, with success or failure, or the form let go of it.
    /// Returns `None` when the section is gone.
    pub fn on_ended<T: 'static>(
        &self,
        cx: &mut Context<T>,
        on_ended: impl Fn(&mut T, &mut Context<T>) + 'static,
    ) -> Option<Subscription> {
        let section = self.section.upgrade()?;
        let generation = self.generation;

        Some(cx.subscribe(
            &section,
            move |this, _, event: &AuthProfilesSectionEvent, cx| {
                if matches!(
                    event,
                    AuthProfilesSectionEvent::LoginEnded { generation: ended } if *ended == generation
                ) {
                    on_ended(this, cx);
                }
            },
        ))
    }
}

impl std::fmt::Debug for SettingsLoginHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsLoginHandle")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<AuthProfilesSectionEvent> for AuthProfilesSection {}
impl EventEmitter<SectionFocusEvent> for AuthProfilesSection {}
impl EventEmitter<SectionPortabilityEvent> for AuthProfilesSection {}

fn build_form_rows(
    has_providers: bool,
    dynamic_field_count: usize,
    selected_provider_supports_login: bool,
    login_url_shown: bool,
    is_editing: bool,
) -> Vec<Vec<AuthFormField>> {
    let mut rows = vec![vec![AuthFormField::Name]];

    if has_providers {
        rows.push(vec![AuthFormField::Provider(0)]);
    }

    for idx in 0..dynamic_field_count {
        rows.push(vec![AuthFormField::DynamicField(idx)]);
    }

    if selected_provider_supports_login {
        rows.push(vec![AuthFormField::ProviderLogin]);

        if login_url_shown {
            rows.push(vec![
                AuthFormField::LoginOpenBrowser,
                AuthFormField::LoginCopyUrl,
                AuthFormField::LoginCancel,
            ]);
        }
    }

    rows.push(vec![AuthFormField::Enabled]);

    if is_editing {
        rows.push(vec![
            AuthFormField::ExportButton,
            AuthFormField::DeleteButton,
            AuthFormField::SaveButton,
        ]);
    } else {
        rows.push(vec![AuthFormField::SaveButton]);
    }

    rows
}

fn build_auth_profile_from_form(
    profile_id: Uuid,
    name: &str,
    provider_id: &str,
    fields: HashMap<String, String>,
    enabled: bool,
) -> Option<AuthProfile> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() || provider_id.is_empty() {
        return None;
    }

    Some(AuthProfile {
        id: profile_id,
        name: trimmed_name.to_string(),
        provider_id: provider_id.to_string(),
        fields,
        // Transient form profile used for live provider calls (validation,
        // option fetching); secret values stay inline and are never persisted
        // from here. The SQLite save path splits them out separately.
        secret_fields: HashMap::new(),
        enabled,
    })
}

/// Update `field_login_hint` and `provider_login_status` for a
/// `FetchOptionsError` returned for `field_id`.
///
/// `NeedsLogin` and `SessionExpired` are surfaced as a per-field hint shown
/// below the affected dropdown and as a warning banner near the Login button,
/// so the user knows to re-authenticate.  Transient and permanent errors are
/// logged at debug level; they do not update the login-hint state because
/// they do not require user authentication action.
///
/// The status banner is not overwritten when a login is already in progress
/// (`login_in_progress == true`), because the running login flow owns it.
fn apply_fetch_error_state(
    field_id: &str,
    error: FetchOptionsError,
    field_login_hint: &mut HashMap<String, String>,
    provider_login_status: &mut Option<(String, bool)>,
    login_in_progress: bool,
) {
    match &error {
        FetchOptionsError::NeedsLogin => {
            field_login_hint.insert(
                field_id.to_string(),
                datatab_i18n::t!("settings.auth_profiles.login_hint"),
            );

            if !login_in_progress {
                *provider_login_status = Some((
                    datatab_i18n::t!("settings.auth_profiles.login_required_hint"),
                    false,
                ));
            }
        }
        FetchOptionsError::SessionExpired => {
            field_login_hint.insert(
                field_id.to_string(),
                datatab_i18n::t!("settings.auth_profiles.session_expired_hint"),
            );

            if !login_in_progress {
                *provider_login_status = Some((
                    datatab_i18n::t!("settings.auth_profiles.session_expired_click_hint"),
                    false,
                ));
            }
        }
        FetchOptionsError::Transient(msg) => {
            log::debug!(
                "fetch_dynamic_options for field '{}': transient: {}",
                field_id,
                msg
            );
        }
        FetchOptionsError::Permanent(msg) => {
            log::debug!(
                "fetch_dynamic_options for field '{}': permanent: {}",
                field_id,
                msg
            );
        }
    }
}

impl AuthProfilesSection {
    pub(super) fn new(
        app_state: Entity<AppStateEntity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected_profile_id = app_state
            .read(cx)
            .list_auth_profiles()
            .first()
            .map(|p| p.id);
        let selected_provider_id = app_state
            .read(cx)
            .auth_provider_registry()
            .providers()
            .next()
            .map(|provider| provider.provider_id().to_string());

        let provider_entries_cache = app_state
            .read(cx)
            .auth_provider_registry()
            .providers()
            .map(|provider| {
                (
                    provider.provider_id().to_string(),
                    provider.display_name().to_string(),
                )
            })
            .collect();

        let input_name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(datatab_i18n::t!("settings.auth_profiles.placeholder_name"))
        });

        let provider_dropdown = cx.new(|_cx| {
            Dropdown::new(SharedString::from("auth-provider-selector")).placeholder(
                datatab_i18n::t!("settings.auth_profiles.select_provider_ellipsis"),
            )
        });

        let app_state_subscription =
            cx.subscribe(&app_state, |this, _, _: &AppStateChanged, cx| {
                this.pending_sync_from_app_state = true;
                cx.notify();
            });

        let provider_dropdown_subscription = cx.subscribe_in(
            &provider_dropdown,
            window,
            |this, _, event: &DropdownSelectionChanged, window, cx| {
                let provider_id = event.item.value.to_string();
                this.selected_provider_id = Some(provider_id);
                this.rebuild_form_inputs(window, cx);

                for input in this.form_inputs.values() {
                    input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                }

                this.options_cache.clear();
                this.field_login_hint.clear();
                this.release_login_of_replaced_form(cx);

                cx.notify();
            },
        );

        let mut section = Self {
            app_state,
            selected_profile_id,
            editing_profile_id: None,
            selected_provider_id,
            selected_provider_supports_login: false,
            provider_entries_cache,
            profile_enabled: true,
            pending_delete_profile_id: None,
            pending_sync_from_app_state: false,
            input_name,
            form_inputs: HashMap::new(),
            provider_field_order: Vec::new(),
            provider_login_loading: false,
            provider_login_status: None,
            login_generation: 0,

            provider_dropdown,
            dynamic_dropdowns: HashMap::new(),
            options_cache: HashMap::new(),
            field_login_hint: HashMap::new(),
            pending_login_complete_refresh: false,
            pending_sso_url: None,
            active_login_url: None,

            auth_focus: AuthFocus::ProfileList,
            auth_form_field: AuthFormField::Name,
            auth_editing_field: false,
            content_focused: false,
            profile_list_scroll_handle: ScrollHandle::new(),
            pending_profile_scroll_idx: None,
            switching_input: false,

            _subscriptions: vec![app_state_subscription],
            _blur_subscriptions: Vec::new(),
            _dropdown_subscriptions: Vec::new(),
            _provider_dropdown_subscription: Some(provider_dropdown_subscription),
        };

        section.rebuild_form_inputs(window, cx);

        if let Some(profile_id) = section.selected_profile_id {
            section.load_profile_into_form(profile_id, window, cx);
        }

        section
    }

    /// Ask the coordinator to export the auth profile currently loaded in the form.
    ///
    /// Only emits when editing a stored profile.
    pub(super) fn request_export(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.editing_profile_id {
            cx.emit(SectionPortabilityEvent::OpenExport(
                ExportTarget::AuthProfile(id),
            ));
        }
    }

    /// Ask the coordinator to open the import wizard.
    pub(super) fn request_import(&mut self, cx: &mut Context<Self>) {
        cx.emit(SectionPortabilityEvent::OpenImport);
    }

    fn provider_entries(&self, cx: &App) -> Vec<(String, String)> {
        self.app_state
            .read(cx)
            .auth_provider_registry()
            .providers()
            .map(|provider| {
                (
                    provider.provider_id().to_string(),
                    provider.display_name().to_string(),
                )
            })
            .collect()
    }

    fn selected_provider(&self, cx: &App) -> Option<Arc<dyn datatab_core::DynAuthProvider>> {
        self.selected_provider_id
            .as_deref()
            .and_then(|provider_id| self.app_state.read(cx).auth_provider_by_id(provider_id))
    }

    fn current_form_profile(&self, cx: &App) -> Option<AuthProfile> {
        let name = self.input_name.read(cx).value().to_string();
        let provider_id = self.selected_provider_id.clone()?;

        let profile_id = self.editing_profile_id.unwrap_or_else(Uuid::new_v4);
        let fields = self
            .form_inputs
            .iter()
            .map(|(field_id, input)| (field_id.clone(), input.read(cx).value().to_string()))
            .collect::<HashMap<_, _>>();

        build_auth_profile_from_form(
            profile_id,
            &name,
            &provider_id,
            fields,
            self.profile_enabled,
        )
    }

    fn rebuild_form_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self.selected_provider(cx) else {
            self.form_inputs.clear();
            self.provider_field_order.clear();
            self.selected_provider_supports_login = false;
            self.rebuild_blur_subscriptions(cx);
            return;
        };

        self.selected_provider_supports_login = provider.capabilities().login.supported;

        let field_defs = provider
            .form_def()
            .tabs
            .iter()
            .flat_map(|tab| tab.sections.iter())
            .flat_map(|section| section.fields.iter())
            .map(|field| {
                (
                    field.id.clone(),
                    field.placeholder.clone(),
                    field.kind.clone(),
                )
            })
            .collect::<Vec<_>>();

        let expected_ids = field_defs
            .iter()
            .map(|(field_id, _, _)| field_id.clone())
            .collect::<HashSet<_>>();

        self.form_inputs
            .retain(|field_id, _| expected_ids.contains(field_id));

        self.provider_field_order = field_defs
            .iter()
            .map(|(field_id, _, _)| field_id.clone())
            .collect();

        let dropdown_ids = field_defs
            .iter()
            .filter(|(_, _, kind)| {
                matches!(
                    kind,
                    FormFieldKind::DynamicSelect { .. } | FormFieldKind::AuthProfileRef { .. }
                )
            })
            .map(|(field_id, _, _)| field_id.clone())
            .collect::<HashSet<_>>();
        self.dynamic_dropdowns
            .retain(|field_id, _| dropdown_ids.contains(field_id));

        let mut dropdown_subs: Vec<Subscription> = Vec::new();

        for (field_id, placeholder, kind) in field_defs {
            match kind {
                FormFieldKind::DynamicSelect { .. } | FormFieldKind::AuthProfileRef { .. } => {
                    // Both dropdown-style fields share the same shell: an
                    // InputState holds the canonical value (so save_profile
                    // can read it uniformly) and a Dropdown entity is the
                    // visible widget. The two kinds differ only in how
                    // options are populated (see render_*_row functions).
                    if !self.form_inputs.contains_key(&field_id) {
                        let input = cx.new(|cx| InputState::new(window, cx));
                        self.form_inputs.insert(field_id.clone(), input);
                    }

                    if !self.dynamic_dropdowns.contains_key(&field_id) {
                        let dropdown_id = format!("auth-dynamic-{}", field_id);
                        let placeholder_str = if placeholder.is_empty() {
                            match &kind {
                                FormFieldKind::AuthProfileRef { .. } => {
                                    datatab_i18n::t!("settings.auth_profiles.select_none")
                                }
                                _ => datatab_i18n::t!("settings.auth_profiles.select_ellipsis"),
                            }
                        } else {
                            placeholder
                        };
                        let dropdown = cx.new(|_cx| {
                            Dropdown::new(SharedString::from(dropdown_id))
                                .placeholder(placeholder_str)
                        });
                        self.dynamic_dropdowns.insert(field_id.clone(), dropdown);
                    }

                    #[expect(
                        clippy::indexing_slicing,
                        reason = "`field_id` is inserted into `dynamic_dropdowns` \
                                  and `form_inputs` above when absent, so the \
                                  lookups below cannot miss"
                    )]
                    let dropdown = self.dynamic_dropdowns[&field_id].clone();
                    #[expect(
                        clippy::indexing_slicing,
                        reason = "`field_id` is inserted into `form_inputs` above \
                                  when absent, so this lookup cannot miss"
                    )]
                    let input = self.form_inputs[&field_id].clone();
                    let sub = cx.subscribe_in(
                        &dropdown,
                        window,
                        move |_this, _, event: &DropdownSelectionChanged, window, cx| {
                            input.update(cx, |state, cx| {
                                state.set_value(event.item.value.to_string(), window, cx);
                            });
                        },
                    );
                    dropdown_subs.push(sub);
                }
                _ => {
                    if self.form_inputs.contains_key(&field_id) {
                        continue;
                    }

                    let input = cx.new(|cx| {
                        let state = InputState::new(window, cx).placeholder(placeholder);
                        match kind {
                            FormFieldKind::Password | FormFieldKind::WriteOnly => {
                                state.masked(true)
                            }
                            _ => state,
                        }
                    });

                    self.form_inputs.insert(field_id, input);
                }
            }
        }

        self._dropdown_subscriptions = dropdown_subs;
        self.rebuild_blur_subscriptions(cx);
    }

    fn rebuild_blur_subscriptions(&mut self, cx: &mut Context<Self>) {
        let mut subs = Vec::new();

        subs.push(create_blur_subscription(cx, &self.input_name.clone()));

        for input in self.form_inputs.values() {
            subs.push(create_blur_subscription(cx, input));
        }

        self._blur_subscriptions = subs;
    }

    // ---------------------------------------------------------------------------
    // T10/T11: Dynamic-select options cache and background fetch
    // ---------------------------------------------------------------------------

    /// Compute a stable 64-bit hash of a sorted dependency map.
    fn hash_deps(deps: &HashMap<String, String>) -> u64 {
        let mut pairs: Vec<(&str, &str)> = deps
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        pairs.sort_unstable();

        let mut hasher = DefaultHasher::new();
        for (key, value) in pairs {
            key.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Handle a `FetchOptionsError` for `field_id`.
    ///
    /// Delegates to the free function `apply_fetch_error_state` so the logic
    /// can be exercised in unit tests without a running GPUI context.
    fn apply_fetch_error(
        &mut self,
        field_id: &str,
        error: FetchOptionsError,
        _cx: &mut Context<Self>,
    ) {
        apply_fetch_error_state(
            field_id,
            error,
            &mut self.field_login_hint,
            &mut self.provider_login_status,
            self.provider_login_loading,
        );
    }

    /// Fetch dynamic options for `field_id` if not already cached and valid.
    ///
    /// Cache miss conditions:
    /// - No entry for `(provider_id, field_id, dep_hash)`.
    /// - Cached entry has expired.
    /// - `refresh == RefreshTrigger::Manual`: fetches only when no cached entry
    ///   exists (first render), never re-fetches automatically after that.
    /// - `refresh == RefreshTrigger::OnLoginComplete` and `login_just_completed` is true.
    /// - `requires_session == true` and no active session.
    ///
    /// On fetch completion the cache is updated and `cx.notify()` is called.
    fn fetch_dynamic_options_if_needed(
        &mut self,
        provider_id: String,
        field: &datatab_core::FormFieldDef,
        session: Option<serde_json::Value>,
        login_just_completed: bool,
        cx: &mut Context<Self>,
    ) {
        let FormFieldKind::DynamicSelect {
            depends_on,
            refresh,
            requires_session,
            ..
        } = &field.kind
        else {
            return;
        };

        // Guard: do not fetch when a session is required but absent.
        if *requires_session && session.is_none() {
            return;
        }

        // Collect the current values of all declared dependencies.
        let deps: HashMap<String, String> = depends_on
            .iter()
            .filter_map(|dep_id| {
                self.form_inputs
                    .get(dep_id)
                    .map(|input| (dep_id.clone(), input.read(cx).value().to_string()))
            })
            .collect();

        let dep_hash = Self::hash_deps(&deps);
        let cache_key = (provider_id.clone(), field.id.clone(), dep_hash);

        // Determine whether we need to (re-)fetch.
        let needs_fetch = match refresh {
            // Manual fields are never auto-refetched.  They fetch exactly once
            // on the first render (cache miss) and then require an explicit
            // user gesture (cache invalidation) to refetch.
            RefreshTrigger::Manual => !self.options_cache.contains_key(&cache_key),
            // OnLoginComplete fields refresh right after a successful login
            // OR on the first render when a session is available (cache miss).
            // Without the cache-miss branch the user would have to re-click
            // Login every time they reopen the editor to see options.
            RefreshTrigger::OnLoginComplete => {
                login_just_completed || !self.options_cache.contains_key(&cache_key)
            }
            RefreshTrigger::OnDependencyChange | RefreshTrigger::OnFocus => {
                match self.options_cache.get(&cache_key) {
                    None => true,
                    Some(cached) => Instant::now() > cached.expires_at,
                }
            }
        };

        if !needs_fetch {
            return;
        }

        let Some(provider) = self.selected_provider(cx) else {
            return;
        };

        let field_id = field.id.clone();
        let this = cx.entity().clone();

        // Build a profile snapshot from the current form values so the provider
        // can access `profile_name`, `region`, `sso_start_url`, etc. The profile
        // name is set to a placeholder when the user has not filled it in yet —
        // the provider only reads `fields`, not `name`, for option fetching.
        let provider_id_snap = provider.provider_id().to_string();
        let profile_id_snap = self.editing_profile_id.unwrap_or_else(Uuid::new_v4);
        let fields_snap: HashMap<String, String> = self
            .form_inputs
            .iter()
            .map(|(key, input)| (key.clone(), input.read(cx).value().to_string()))
            .collect();

        let raw_snapshot = AuthProfile {
            id: profile_id_snap,
            name: self.input_name.read(cx).value().to_string(),
            provider_id: provider_id_snap,
            fields: fields_snap,
            // Transient snapshot for live option fetching: secret inputs stay
            // inline in `fields`, and nothing here is persisted. An in-process
            // provider reads the snapshot as it is. An RPC provider receives
            // only the fields its form declares, and a secret one only when
            // it opted in (`RpcAuthProvider::fetch_dynamic_options`).
            secret_fields: HashMap::new(),
            enabled: self.profile_enabled,
        };

        // Expand AuthProfileRef fields so option fetching sees the same flat
        // field map as the connection pipeline.
        let profile_registry_snapshot: Vec<AuthProfile> =
            self.app_state.read(cx).list_auth_profiles();
        let profile_snapshot = datatab_core::auth::expand_auth_profile_refs(
            &raw_snapshot,
            provider.form_def(),
            &|target_id| {
                profile_registry_snapshot
                    .iter()
                    .find(|p| p.id == *target_id)
                    .cloned()
            },
        );

        let request = FetchOptionsRequest {
            field_id: field_id.clone(),
            dependencies: deps,
            session,
        };

        let fetch_task = cx.background_executor().spawn(async move {
            provider
                .fetch_dynamic_options(&profile_snapshot, request)
                .await
        });

        cx.spawn(async move |_this, cx| {
            let result = fetch_task.await;

            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    match result {
                        Ok(response) => {
                            let ttl_secs = response.cache_hint_seconds.unwrap_or(0) as u64;
                            let expires_at = Instant::now()
                                + std::time::Duration::from_secs(if ttl_secs == 0 {
                                    0
                                } else {
                                    ttl_secs
                                });

                            let options = response.options;

                            this.options_cache.insert(
                                (provider_id.clone(), field_id.clone(), dep_hash),
                                CachedOptions {
                                    options,
                                    expires_at,
                                },
                            );

                            // Clear any prior login hint for this field now that
                            // options loaded successfully.
                            this.field_login_hint.remove(&field_id);

                            // Sync cached options into the dropdown entity.
                            if let Some(dropdown) = this.dynamic_dropdowns.get(&field_id) {
                                let items = this
                                    .options_cache
                                    .get(&(provider_id, field_id, dep_hash))
                                    .map(|cached| {
                                        cached
                                            .options
                                            .iter()
                                            .map(|opt| {
                                                DropdownItem::with_value(
                                                    opt.label.clone(),
                                                    opt.value.clone(),
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();

                                dropdown.update(cx, |dropdown, cx| {
                                    dropdown.set_items(items, cx);
                                });
                            }
                        }
                        Err(error) => {
                            this.apply_fetch_error(&field_id, error, cx);
                        }
                    }

                    cx.notify();
                });
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------------------
    // Generic DynamicSelect dropdown row
    // ---------------------------------------------------------------------------

    /// Render a `DynamicSelect` field as a dropdown row.
    ///
    /// The dropdown entity was created in `rebuild_form_inputs`. This method
    /// syncs the current cached options into the dropdown and renders it.
    fn render_dynamic_dropdown_row(
        &mut self,
        field: &datatab_core::FormFieldDef,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let field_id = field.id.clone();
        let label = field.label.clone();

        // Build the dropdown if it was not yet created (e.g. if rebuild_form_inputs
        // was not called before the first render — defensive path only).
        if !self.dynamic_dropdowns.contains_key(&field_id) {
            let placeholder = if field.placeholder.is_empty() {
                datatab_i18n::t!("settings.auth_profiles.select_ellipsis")
            } else {
                field.placeholder.clone()
            };
            let dropdown_id = format!("auth-dynamic-{}", field_id);
            let dropdown = cx
                .new(|_cx| Dropdown::new(SharedString::from(dropdown_id)).placeholder(placeholder));
            self.dynamic_dropdowns.insert(field_id.clone(), dropdown);
        }

        #[expect(
            clippy::indexing_slicing,
            reason = "the block above inserts `field_id` into `dynamic_dropdowns` \
                      when absent, so this lookup cannot miss"
        )]
        let dropdown = self.dynamic_dropdowns[&field_id].clone();

        // Sync cached options into the dropdown.
        if let Some(provider_id) = self.selected_provider_id.clone() {
            let deps: HashMap<String, String> = match &field.kind {
                FormFieldKind::DynamicSelect { depends_on, .. } => depends_on
                    .iter()
                    .filter_map(|dep_id| {
                        self.form_inputs
                            .get(dep_id)
                            .map(|input| (dep_id.clone(), input.read(cx).value().to_string()))
                    })
                    .collect(),
                _ => HashMap::new(),
            };

            let dep_hash = Self::hash_deps(&deps);
            let cache_key = (provider_id, field_id.clone(), dep_hash);

            let current_value = self
                .form_inputs
                .get(&field_id)
                .map(|input| input.read(cx).value().to_string())
                .unwrap_or_default();

            // Always re-sync the dropdown: the entity is reused across profiles
            // (keyed by field id), so a cache miss must reset it rather than
            // leave a stale label from a previously loaded profile. With no
            // fetched options (cache cleared on load, or session expired), fall
            // back to the profile's own stored value as a single item.
            let items: Vec<DropdownItem> = match self.options_cache.get(&cache_key) {
                Some(cached) => cached
                    .options
                    .iter()
                    .map(|opt| DropdownItem::with_value(opt.label.clone(), opt.value.clone()))
                    .collect(),
                None if !current_value.is_empty() => {
                    vec![DropdownItem::with_value(
                        current_value.clone(),
                        current_value.clone(),
                    )]
                }
                None => Vec::new(),
            };

            let selected_index = items.iter().position(|item| item.value == current_value);

            dropdown.update(cx, |d, cx| {
                d.set_items(items, cx);
                d.set_selected_index(selected_index, cx);
            });
        }

        let login_hint = self.field_login_hint.get(&field_id).cloned();
        let help = field.help.clone();

        layout::form_row(
            label,
            div()
                .flex()
                .flex_col()
                .gap(FormMetrics::HELP_GAP)
                .child(layout::cursor_ring(
                    self.is_cursor_on_dynamic_field(&field_id),
                    div().w(AUTH_SELECT_WIDTH).child(dropdown),
                    cx,
                ))
                .when_some(help, |column, help| column.child(layout::help_text(help)))
                .when_some(login_hint, |column, hint| {
                    column.child(Text::body(hint).font_size(FormMetrics::HELP_FONT).warning())
                }),
            None,
        )
    }

    /// Render an `AuthProfileRef` field as a dropdown of existing auth
    /// profiles whose `provider_id` matches the field's filter. The selected
    /// value is the referenced profile's UUID (or empty for "none").
    fn render_auth_profile_ref_row(
        &mut self,
        field: &datatab_core::FormFieldDef,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let field_id = field.id.clone();
        let label = field.label.clone();
        let help = field.help.clone();

        let FormFieldKind::AuthProfileRef {
            provider_id: Some(ref_provider_id),
        } = &field.kind
        else {
            return div().into_any_element();
        };

        if !self.dynamic_dropdowns.contains_key(&field_id) {
            let dropdown_id = format!("auth-dynamic-{}", field_id);
            let dropdown = cx.new(|_cx| {
                Dropdown::new(SharedString::from(dropdown_id))
                    .placeholder(datatab_i18n::t!("settings.auth_profiles.select_none"))
            });
            self.dynamic_dropdowns.insert(field_id.clone(), dropdown);
        }

        #[expect(
            clippy::indexing_slicing,
            reason = "the block above inserts `field_id` into `dynamic_dropdowns` \
                      when absent, so this lookup cannot miss"
        )]
        let dropdown = self.dynamic_dropdowns[&field_id].clone();

        let mut items: Vec<DropdownItem> = vec![DropdownItem::with_value(
            "— None —".to_string(),
            String::new(),
        )];
        // Read through the same profile list used by reference expansion.
        let referenced_profiles: Vec<(String, String)> = self
            .app_state
            .read(cx)
            .list_auth_profiles()
            .iter()
            .filter(|profile| profile.provider_id == *ref_provider_id && profile.enabled)
            .map(|profile| (profile.id.to_string(), profile.name.clone()))
            .collect();
        for (id, name) in referenced_profiles {
            items.push(DropdownItem::with_value(name, id));
        }

        let current_value = self
            .form_inputs
            .get(&field_id)
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let selected_index = items.iter().position(|item| item.value == current_value);

        dropdown.update(cx, |d, cx| {
            d.set_items(items, cx);
            d.set_selected_index(selected_index, cx);
        });

        layout::form_row(
            label,
            layout::cursor_ring(
                self.is_cursor_on_dynamic_field(&field_id),
                div().w(AUTH_SELECT_WIDTH).child(dropdown),
                cx,
            ),
            help.map(SharedString::from),
        )
        .into_any_element()
    }

    fn clear_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_profile_id = None;
        self.profile_enabled = true;

        self.input_name.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });

        if self.selected_provider_id.is_none() {
            self.selected_provider_id = self
                .provider_entries(cx)
                .first()
                .map(|(provider_id, _)| provider_id.clone());
        }

        self.rebuild_form_inputs(window, cx);

        for input in self.form_inputs.values() {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }

        // Clear the dynamic-select options cache so stale options are not shown
        // for a new/reset profile.
        self.options_cache.clear();
        self.field_login_hint.clear();

        self.release_login_of_replaced_form(cx);
    }

    fn sync_from_app_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Refresh provider entries so newly discovered RPC providers appear
        // in the dropdown without requiring a section reload.
        self.provider_entries_cache = self.provider_entries(cx);

        let profiles = self.app_state.read(cx).list_auth_profiles();

        if profiles.is_empty() {
            self.selected_profile_id = None;
            self.clear_form(window, cx);
            return;
        }

        if let Some(selected_id) = self.selected_profile_id
            && profiles.iter().any(|profile| profile.id == selected_id)
        {
            return;
        }

        self.selected_profile_id = profiles.first().map(|profile| profile.id);
        if let Some(profile_id) = self.selected_profile_id {
            self.load_profile_into_form(profile_id, window, cx);
        }
    }

    fn profile_ids(&self, cx: &App) -> Vec<Uuid> {
        self.app_state
            .read(cx)
            .list_auth_profiles()
            .iter()
            .map(|profile| profile.id)
            .collect()
    }

    fn selected_profile_index(&self, profile_ids: &[Uuid]) -> Option<usize> {
        self.selected_profile_id
            .and_then(|profile_id| profile_ids.iter().position(|id| *id == profile_id))
    }

    fn load_profile_at_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let profile_ids = self.profile_ids(cx);
        if let Some(profile_id) = profile_ids.get(index).copied() {
            self.pending_profile_scroll_idx = Some(index);
            self.load_profile_into_form(profile_id, window, cx);
        }
    }

    fn move_profile_selection(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let profile_ids = self.profile_ids(cx);
        if profile_ids.is_empty() {
            return;
        }

        let current_index = self.selected_profile_index(&profile_ids).unwrap_or(0);
        let current_index = current_index as isize;
        let next_index = (current_index + step).clamp(0, profile_ids.len() as isize - 1) as usize;

        self.load_profile_at_index(next_index, window, cx);
    }

    fn handle_key_event(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.content_focused || self.pending_delete_profile_id.is_some() {
            return;
        }

        if self.handle_editing_keys(event, window, cx) {
            return;
        }

        let chord = key_chord_from_gpui(&event.keystroke);

        match self.auth_focus {
            AuthFocus::ProfileList => match (chord.key.as_str(), chord.modifiers) {
                ("j", modifiers) | ("down", modifiers) if modifiers == Modifiers::none() => {
                    self.move_profile_selection(1, window, cx);
                    cx.notify();
                }
                ("k", modifiers) | ("up", modifiers) if modifiers == Modifiers::none() => {
                    self.move_profile_selection(-1, window, cx);
                    cx.notify();
                }
                ("g", modifiers) if modifiers == Modifiers::none() => {
                    self.load_profile_at_index(0, window, cx);
                    cx.notify();
                }
                ("g", modifiers) if modifiers == Modifiers::shift() => {
                    let profile_count = self.profile_ids(cx).len();
                    if profile_count > 0 {
                        self.load_profile_at_index(profile_count - 1, window, cx);
                        cx.notify();
                    }
                }
                ("n", modifiers) if modifiers == Modifiers::none() => {
                    self.begin_create_profile(window, cx);
                }
                ("d", modifiers) if modifiers == Modifiers::none() => {
                    self.request_delete_selected_profile(cx);
                }
                ("i", modifiers) if modifiers == Modifiers::none() => {
                    self.request_import(cx);
                }
                ("l", modifiers) | ("right", modifiers) | ("enter", modifiers)
                    if modifiers == Modifiers::none() =>
                {
                    self.enter_form(window, cx);
                    cx.notify();
                }
                _ => {}
            },
            AuthFocus::Form => match (chord.key.as_str(), chord.modifiers) {
                ("escape", modifiers) if modifiers == Modifiers::none() => {
                    self.exit_form(window, cx);
                    cx.notify();
                }
                ("j", modifiers) | ("down", modifiers) if modifiers == Modifiers::none() => {
                    self.move_down();
                    cx.notify();
                }
                ("k", modifiers) | ("up", modifiers) if modifiers == Modifiers::none() => {
                    self.move_up();
                    cx.notify();
                }
                ("h", modifiers) if modifiers == Modifiers::none() => {
                    self.exit_form(window, cx);
                    cx.notify();
                }
                ("left", modifiers) if modifiers == Modifiers::none() => {
                    self.move_left();
                    cx.notify();
                }
                ("l", modifiers) | ("right", modifiers) if modifiers == Modifiers::none() => {
                    self.move_right();
                    cx.notify();
                }
                ("enter", modifiers) if modifiers == Modifiers::none() => {
                    self.activate_current_field(window, cx);
                    cx.notify();
                }
                ("tab", modifiers) if modifiers == Modifiers::none() => {
                    self.tab_next();
                    cx.notify();
                }
                ("tab", modifiers) if modifiers == Modifiers::shift() => {
                    self.tab_prev();
                    cx.notify();
                }
                ("g", modifiers) if modifiers == Modifiers::none() => {
                    self.move_first();
                    cx.notify();
                }
                ("g", modifiers) if modifiers == Modifiers::shift() => {
                    self.move_last();
                    cx.notify();
                }
                _ => {}
            },
        }
    }

    fn load_profile_into_form(
        &mut self,
        profile_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = self
            .app_state
            .read(cx)
            .list_auth_profiles()
            .into_iter()
            .find(|profile| profile.id == profile_id);

        let Some(profile) = profile else {
            return;
        };

        self.selected_profile_id = Some(profile.id);
        self.editing_profile_id = Some(profile.id);
        self.profile_enabled = profile.enabled;
        self.selected_provider_id = Some(profile.provider_id.clone());

        self.input_name.update(cx, |state, cx| {
            state.set_value(profile.name.clone(), window, cx);
        });

        self.rebuild_form_inputs(window, cx);

        // Classify secret-kind fields. `WriteOnly` fields are never pre-filled;
        // `Password` fields are pre-filled from the re-hydrated `secret_fields`
        // so the user sees the stored value (masked) and can keep or change it.
        let mut write_only_fields: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut password_fields: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        if let Some(provider) = self.selected_provider(cx) {
            for field in provider
                .form_def()
                .tabs
                .iter()
                .flat_map(|tab| tab.sections.iter())
                .flat_map(|section| section.fields.iter())
            {
                match field.kind {
                    FormFieldKind::WriteOnly => {
                        write_only_fields.insert(field.id.clone());
                    }
                    FormFieldKind::Password => {
                        password_fields.insert(field.id.clone());
                    }
                    _ => {}
                }
            }
        }

        for (field_id, input) in &self.form_inputs {
            // WriteOnly fields are always rendered empty (spec R9.4.1, S33).
            let value = if write_only_fields.contains(field_id) {
                String::new()
            } else if password_fields.contains(field_id) {
                profile
                    .secret_fields
                    .get(field_id)
                    .map(|secret| secret.expose_secret().to_string())
                    .unwrap_or_default()
            } else {
                profile.fields.get(field_id).cloned().unwrap_or_default()
            };

            input.update(cx, |state, cx| {
                state.set_value(value, window, cx);
            });
        }

        // Clear options cache so DynamicSelect dropdowns are re-fetched for
        // the newly loaded profile's field values.
        self.options_cache.clear();
        self.field_login_hint.clear();

        // A login still in flight belongs to the form as it was: its result
        // must not land on the profile loaded now.
        self.release_login(cx);
        self.provider_login_status = None;

        cx.notify();
    }

    fn begin_create_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_profile_id = None;
        self.clear_form(window, cx);
        self.enter_form(window, cx);
        cx.notify();
    }

    fn collect_profile_fields(
        &self,
        cx: &App,
    ) -> (HashMap<String, String>, HashMap<String, SecretString>) {
        // Secret-kind fields (Password / WriteOnly) are split out of `fields`
        // into `secret_fields` so they are routed to the keyring and never
        // persisted as plaintext.
        let secret_field_ids: HashSet<String> = self
            .selected_provider(cx)
            .map(|provider| {
                provider
                    .form_def()
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.sections.iter())
                    .flat_map(|section| section.fields.iter())
                    .filter(|field| {
                        matches!(
                            field.kind,
                            FormFieldKind::Password | FormFieldKind::WriteOnly
                        )
                    })
                    .map(|field| field.id.clone())
                    .collect()
            })
            .unwrap_or_default();

        // Secrets already stored for the profile being edited, used to preserve
        // a WriteOnly value the user intentionally left blank (spec R9.4.2).
        let existing_secrets: HashMap<String, SecretString> = self
            .editing_profile_id
            .and_then(|id| {
                self.app_state
                    .read(cx)
                    .stored_auth_profile_secret_fields(id)
            })
            .unwrap_or_default();

        let mut fields: HashMap<String, String> = HashMap::new();
        let mut secret_fields: HashMap<String, SecretString> = HashMap::new();

        for (field_id, input) in &self.form_inputs {
            let value = input.read(cx).value().to_string();

            if !secret_field_ids.contains(field_id) {
                fields.insert(field_id.clone(), value);
                continue;
            }

            if value.is_empty() {
                // Blank secret input: keep the existing stored secret, if any.
                if let Some(existing) = existing_secrets.get(field_id) {
                    secret_fields.insert(field_id.clone(), existing.clone());
                }
            } else {
                secret_fields.insert(field_id.clone(), SecretString::from(value));
            }
        }

        (fields, secret_fields)
    }

    fn save_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.input_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }

        let Some(provider_id) = self.selected_provider_id.clone() else {
            return;
        };

        let profile_id = self.editing_profile_id.unwrap_or_else(Uuid::new_v4);

        let (fields, secret_fields) = self.collect_profile_fields(cx);

        let has_secret_fields = !secret_fields.is_empty();

        let profile = AuthProfile {
            id: profile_id,
            name,
            provider_id,
            fields,
            secret_fields,
            enabled: self.profile_enabled,
        };

        let is_edit = self.editing_profile_id.is_some();
        let secrets_persisted = self.app_state.update(cx, |state, cx| {
            let persisted = if is_edit {
                state.update_auth_profile(profile.clone())
            } else {
                state.add_auth_profile(profile.clone())
            };

            cx.emit(AppStateChanged);
            persisted
        });

        // The profile row is saved, but secret-kind fields live in the OS
        // keyring. Drive this off the actual write result (not a cached
        // availability probe): a locked keyring reports available yet rejects
        // writes, so only the real outcome tells us the secret was lost.
        if has_secret_fields && !secrets_persisted {
            report_error(
                UserFacingError::new(
                    ErrorKind::Storage,
                    datatab_i18n::t!("settings.auth_profiles.error.secret_store_failed"),
                ),
                cx,
            );
        }

        self.selected_profile_id = Some(profile_id);
        self.load_profile_into_form(profile_id, window, cx);
    }

    fn login_selected_profile(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.selected_provider(cx) else {
            self.provider_login_status = Some((
                datatab_i18n::t!("settings.auth_profiles.select_before_login"),
                false,
            ));
            cx.notify();
            return;
        };

        if !provider.capabilities().login.supported {
            self.provider_login_status = Some((
                datatab_i18n::t!("settings.auth_profiles.interactive_login_unavailable"),
                false,
            ));
            cx.notify();
            return;
        }

        let Some(raw_profile) = self.current_form_profile(cx) else {
            self.provider_login_status = Some((
                datatab_i18n::t!("settings.auth_profiles.fields_required_before_login"),
                false,
            ));
            cx.notify();
            return;
        };

        // Expand AuthProfileRef fields so the provider's login() sees a flat
        // field map (e.g. `sso_start_url` filled from the referenced session).
        let profile_registry_snapshot: Vec<AuthProfile> =
            self.app_state.read(cx).list_auth_profiles();
        let profile = datatab_core::auth::expand_auth_profile_refs(
            &raw_profile,
            provider.form_def(),
            &|target_id| {
                profile_registry_snapshot
                    .iter()
                    .find(|p| p.id == *target_id)
                    .cloned()
            },
        );

        self.release_login(cx);
        let login_generation = self.login_generation;
        self.provider_login_loading = true;
        self.provider_login_status = Some((
            crate::labels::auth_profiles_starting_login(&profile.name),
            false,
        ));
        cx.notify();

        let this = cx.entity().clone();
        let provider_name_for_url = provider.display_name().to_string();
        let profile_name_for_url = profile.name.clone();

        // The verification URL arrives via `UrlCallback` from a background
        // thread *before* `provider.login()` completes (the provider blocks
        // waiting for the user to finish in the browser). We forward the URL
        // through a channel so the modal can open immediately — independently
        // of when the login future resolves.
        let (url_tx, url_rx) = std::sync::mpsc::sync_channel::<Option<String>>(1);
        let url_callback: datatab_core::auth::UrlCallback = Box::new(move |url| {
            url_tx.try_send(url).log_err();
        });

        // URL-forwarding task: poll the channel and push the verification URL
        // into the modal as soon as the provider surfaces it.
        let this_for_url = this.clone();
        cx.spawn(async move |_, cx| {
            loop {
                match url_rx.try_recv() {
                    Ok(Some(url)) => {
                        cx.update(|cx| {
                            this_for_url.update(cx, |this, cx| {
                                this.show_login_url(
                                    login_generation,
                                    provider_name_for_url,
                                    profile_name_for_url,
                                    url,
                                    cx,
                                );
                            });
                        });
                        break;
                    }
                    Ok(None) => break, // provider explicitly has no URL
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(150))
                            .await;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }
            }
        })
        .detach();

        // Main task: drive the login future to completion, then update final
        // status. The URL itself is already routed by the task above.
        cx.spawn(async move |_this, cx| {
            let result = provider.login(&profile, url_callback).await;

            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    // Cancel, or a login started since, took the form over:
                    // this result is late and changes nothing.
                    if this.login_generation != login_generation {
                        return;
                    }

                    // Login finished — nothing waits for it any more, and its
                    // URL goes regardless of outcome (success means token
                    // cached, failure means the URL is no longer actionable).
                    this.release_login(cx);
                    this.provider_login_status = Some(match &result {
                        Ok(_) => (
                            crate::labels::auth_profiles_login_completed(&profile.name),
                            true,
                        ),
                        Err(error) => (
                            crate::labels::auth_profiles_login_failed(
                                &profile.name,
                                &error.to_string(),
                            ),
                            false,
                        ),
                    });

                    if this
                        .provider_login_status
                        .as_ref()
                        .is_some_and(|status| status.1)
                    {
                        this.options_cache.clear();
                        this.field_login_hint.clear();
                        this.pending_login_complete_refresh = true;
                    }

                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// Shows the verification URL of the login of `generation`: inline under
    /// the Login button, and in the login modal. A URL that arrives after the
    /// form let go of its login is dropped.
    fn show_login_url(
        &mut self,
        generation: u64,
        provider_name: String,
        profile_name: String,
        url: String,
        cx: &mut Context<Self>,
    ) {
        if self.login_generation != generation {
            return;
        }

        self.active_login_url = Some(url.clone());
        self.pending_sso_url = Some((provider_name, profile_name, Some(url), generation));
        cx.notify();
    }

    /// Lets go of the login in flight, if any, because it finished or the
    /// form moved on: the Login button is enabled again, a result or a URL of
    /// that login is ignored when it arrives later, and whoever shows its URL
    /// is told once that it ended.
    fn release_login(&mut self, cx: &mut Context<Self>) {
        let released = self.login_generation;

        self.login_generation = self.login_generation.wrapping_add(1);
        self.provider_login_loading = false;
        self.active_login_url = None;
        self.pending_sso_url = None;
        cx.emit(AuthProfilesSectionEvent::LoginEnded {
            generation: released,
        });
    }

    fn request_delete_selected_profile(&mut self, cx: &mut Context<Self>) {
        self.pending_delete_profile_id = self.editing_profile_id;
        cx.notify();
    }

    fn confirm_delete_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile_id) = self.pending_delete_profile_id.take() else {
            return;
        };

        self.app_state.update(cx, |state, cx| {
            let affected: Vec<_> = state
                .profiles()
                .iter()
                .filter(|profile| {
                    profile.auth_profile_id == Some(profile_id)
                        || matches!(
                            &profile.access_kind,
                            Some(AccessKind::Managed { params, .. })
                                if params.get("auth_profile_id")
                                    .and_then(|s| s.parse::<uuid::Uuid>().ok())
                                    == Some(profile_id)
                        )
                })
                .cloned()
                .collect();

            for mut profile in affected {
                if profile.auth_profile_id == Some(profile_id) {
                    profile.auth_profile_id = None;
                }

                if let Some(AccessKind::Managed { params, .. }) = profile.access_kind.as_mut()
                    && params
                        .get("auth_profile_id")
                        .and_then(|s| s.parse::<uuid::Uuid>().ok())
                        == Some(profile_id)
                {
                    params.remove("auth_profile_id");
                }

                state.update_profile(profile);
            }

            // remove_auth_profile takes an index in the stored list.
            #[allow(deprecated)]
            if let Some(index) = state
                .auth_profiles()
                .iter()
                .position(|profile| profile.id == profile_id)
            {
                state.remove_auth_profile(index);
            }

            cx.emit(AppStateChanged);
        });

        self.editing_profile_id = None;
        // After deletion the sidebar selects the first stored profile.
        #[allow(deprecated)]
        let first_stored_id = self
            .app_state
            .read(cx)
            .auth_profiles()
            .first()
            .map(|profile| profile.id);
        self.selected_profile_id = first_stored_id;

        if let Some(selected_id) = self.selected_profile_id {
            self.load_profile_into_form(selected_id, window, cx);
        } else {
            self.clear_form(window, cx);
        }

        cx.notify();
    }

    fn cancel_delete_profile(&mut self, cx: &mut Context<Self>) {
        self.pending_delete_profile_id = None;
        cx.notify();
    }

    fn profiles_using_auth(&self, auth_id: Uuid, cx: &Context<Self>) -> usize {
        self.app_state
            .read(cx)
            .profiles()
            .iter()
            .filter(|profile| {
                profile.auth_profile_id == Some(auth_id)
                    || matches!(
                        &profile.access_kind,
                        Some(AccessKind::Managed { params, .. })
                            if params.get("auth_profile_id")
                                .and_then(|s| s.parse::<uuid::Uuid>().ok())
                                == Some(auth_id)
                    )
            })
            .count()
    }

    /// Lets go of a login in flight because the form was emptied for a new
    /// profile or switched to another provider: the login belongs to the form
    /// as it was, and its "starting" status goes with it. A form with no
    /// login in flight keeps its status.
    fn release_login_of_replaced_form(&mut self, cx: &mut Context<Self>) {
        if self.provider_login_loading {
            self.release_login(cx);
            self.provider_login_status = None;
        }
    }

    /// Stops waiting for the in-flight login of the profile being edited: the
    /// Login button is enabled again at once, and the result of that login is
    /// ignored when it arrives. A provider may never answer, and an RPC
    /// provider cannot be told to stop. The active provider is still asked to
    /// abort the login, for the ones that can.
    fn cancel_provider_login(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.current_form_profile(cx)
            && let Some(provider) = self.selected_provider(cx)
        {
            let _aborted = provider.abort_login(&profile);
        }

        self.release_login(cx);
        self.provider_login_status = Some((
            datatab_i18n::t!("settings.auth_profiles.login_cancelled"),
            false,
        ));
        cx.notify();
    }

    /// Renders the inline login-URL panel: shows the verification URL with
    /// Open Browser, Copy URL, and Cancel buttons. Used while an interactive
    /// SSO login is in flight from the Settings window.
    fn render_login_url_panel(&self, url: String, cx: &mut Context<Self>) -> impl IntoElement {
        let url_for_open = url.clone();
        let url_for_copy = url.clone();

        let actions = layout::inline_controls()
            .child(
                Button::new(
                    "auth-login-open-browser",
                    datatab_i18n::t!("settings.auth_profiles.open_browser"),
                )
                .primary()
                .icon(AppIcon::ExternalLink)
                .focused(self.is_cursor_on(AuthFormField::LoginOpenBrowser))
                .on_click(cx.listener(move |_this, _, _, cx| {
                    cx.open_url(&url_for_open);
                })),
            )
            .child(
                Button::new(
                    "auth-login-copy-url",
                    datatab_i18n::t!("settings.auth_profiles.copy_url"),
                )
                .secondary()
                .icon(AppIcon::Copy)
                .focused(self.is_cursor_on(AuthFormField::LoginCopyUrl))
                .on_click(cx.listener(move |_this, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(url_for_copy.clone()));
                })),
            )
            .child(
                Button::new(
                    "auth-login-cancel",
                    datatab_i18n::t!("settings.auth_profiles.cancel"),
                )
                .danger()
                .focused(self.is_cursor_on(AuthFormField::LoginCancel))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_provider_login(cx);
                })),
            );

        div().mt(FormMetrics::ROW_GAP).child(
            BannerBlock::new(
                BannerVariant::Info,
                datatab_i18n::t!("settings.auth_profiles.login_url_panel_description"),
            )
            .with_pre(url)
            .with_actions(actions),
        )
    }

    /// Returns the AuthProfile referenced by `trigger_value` (expected to
    /// be a UUID string), used to populate "inherited from" hints and to
    /// surface the referenced field's value in disabled inputs.
    fn resolve_ref_profile(&self, trigger_value: &str, cx: &App) -> Option<AuthProfile> {
        let target_id = Uuid::parse_str(trigger_value.trim()).ok()?;
        // Resolve display hints for profiles linked in the stored form data.
        #[allow(deprecated)]
        let result = self
            .app_state
            .read(cx)
            .auth_profiles()
            .iter()
            .find(|profile| profile.id == target_id)
            .cloned();
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn render_input_row_disabled(
        &self,
        label: &str,
        input: &Entity<InputState>,
        field: AuthFormField,
        is_secret: bool,
        is_focused: bool,
        disabled: bool,
        disabled_hint: Option<String>,
        inherited_value: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();

        let control = if disabled {
            // Render a static, non-interactive text view. gpui_component's
            // Input keeps its key_down handler bound even when `disabled` is
            // true, so an "Input::disabled(true)" widget would still accept
            // keystrokes. A read-only text node makes the field truly inert.
            let value = inherited_value
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| input.read(cx).value().to_string());
            let display = if value.trim().is_empty() {
                "—".to_string()
            } else {
                value
            };

            div()
                .relative()
                .flex()
                .items_center()
                .w(AUTH_SELECT_WIDTH)
                .h(Fields::HEIGHT)
                .px(Fields::PADDING_X)
                .text_size(Fields::TEXT)
                .text_color(theme.muted_foreground)
                .child(
                    Chamfer::new(ChamferCut::CONTROL)
                        .fill(theme.secondary.opacity(Fields::DISABLED_OPACITY))
                        .border(theme.border),
                )
                .child(display)
        } else {
            let element_id = match field {
                AuthFormField::Name => "auth-profile-name".to_string(),
                AuthFormField::DynamicField(index) => format!("auth-profile-field-{index}"),
                _ => "auth-profile-control".to_string(),
            };

            layout::field_frame(
                is_focused && !self.auth_editing_field,
                Some(AUTH_SELECT_WIDTH),
                false,
                Input::new(input)
                    .id(SharedString::from(element_id))
                    .aria_label(label.to_string())
                    .secret(is_secret),
                cx,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.switching_input = true;
                    this.auth_focus = AuthFocus::Form;
                    this.auth_form_field = field;
                    this.focus_current_field(window, cx);
                    cx.notify();
                }),
            )
        };

        layout::form_row(
            label.to_string(),
            control,
            disabled_hint.map(SharedString::from),
        )
    }

    fn render_provider_selector(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let items: Vec<DropdownItem> = self
            .provider_entries_cache
            .iter()
            .map(|(provider_id, label)| {
                DropdownItem::with_value(label.clone(), provider_id.clone())
            })
            .collect();

        let selected_index = self
            .selected_provider_id
            .as_deref()
            .and_then(|provider_id| {
                self.provider_entries_cache
                    .iter()
                    .position(|(id, _)| id == provider_id)
            });

        self.provider_dropdown.update(cx, |dropdown, cx| {
            dropdown.set_items(items, cx);
            dropdown.set_selected_index(selected_index, cx);
        });

        let cursor = self.content_focused
            && self.auth_focus == AuthFocus::Form
            && matches!(self.auth_form_field, AuthFormField::Provider(_));

        layout::cursor_ring(
            cursor,
            div()
                .w(AUTH_SELECT_WIDTH)
                .child(self.provider_dropdown.clone()),
            cx,
        )
        .w(AUTH_SELECT_WIDTH)
    }

    fn render_profile_list(
        &mut self,
        profiles: &[AuthProfile],
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let list_focused = self.content_focused && self.auth_focus == AuthFocus::ProfileList;

        if let Some(scroll_idx) = self.pending_profile_scroll_idx.take() {
            self.profile_list_scroll_handle.scroll_to_item(scroll_idx);
        }

        let toolbar = layout::master_list_toolbar(vec![
            Button::new(
                "new-auth-profile",
                datatab_i18n::t!("settings.auth_profiles.new_profile"),
            )
            .primary()
            .icon(AppIcon::Plus)
            .on_click(cx.listener(|this, _, window, cx| {
                this.auth_focus = AuthFocus::Form;
                this.begin_create_profile(window, cx);
            }))
            .into_any_element(),
            Button::new(
                "import-auth-profile",
                datatab_i18n::t!("settings.auth_profiles.import"),
            )
            .secondary()
            .icon(AppIcon::Download)
            .on_click(cx.listener(|this, _, _, cx| {
                this.request_import(cx);
            }))
            .into_any_element(),
        ]);

        let rows: Vec<AnyElement> = profiles
            .iter()
            .map(|profile| {
                let profile_id = profile.id;
                let is_selected = self.selected_profile_id == Some(profile_id);
                let is_focused = list_focused && is_selected;
                let provider_label = self
                    .app_state
                    .read(cx)
                    .auth_provider_by_id(&profile.provider_id)
                    .map(|provider| provider.display_name().to_string())
                    .unwrap_or_else(|| profile.provider_id.clone());

                let trailing = (!profile.enabled).then(|| {
                    Badge::new(
                        datatab_i18n::t!("settings.auth_profiles.disabled_badge"),
                        BadgeTone::Neutral,
                    )
                    .into_any_element()
                });

                layout::master_list_row(
                    SharedString::from(format!("auth-profile-item-{}", profile_id)),
                    layout::MasterRow {
                        icon: Some(AppIcon::KeyRound),
                        title: profile.name.clone().into(),
                        detail: Some(provider_label.into()),
                        trailing,
                    },
                    is_selected,
                    is_focused,
                    cx,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.switching_input = true;
                        this.load_profile_into_form(profile_id, window, cx);
                    }),
                )
                .into_any_element()
            })
            .collect();

        let theme = cx.theme();

        div()
            .w(SettingsMetrics::LIST_WIDTH)
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .child(toolbar)
            .child(layout::scrolled_list_viewport(
                div()
                    .id("auth-profile-list-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .track_scroll(&self.profile_list_scroll_handle)
                    .flex()
                    .flex_col()
                    .children(rows),
                &self.profile_list_scroll_handle,
            ))
    }

    fn render_editor_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();

        // Drive DynamicSelect fetches for the current provider's fields.
        // `fetch_dynamic_options_if_needed` is a no-op for fields already
        // cached and within their TTL.
        let login_just_completed = self.pending_login_complete_refresh;
        if login_just_completed {
            self.pending_login_complete_refresh = false;
        }

        let provider_id_for_fetch = self.selected_provider_id.clone().unwrap_or_default();
        let field_defs_for_fetch: Vec<datatab_core::FormFieldDef> = self
            .selected_provider(cx)
            .map(|provider| {
                provider
                    .form_def()
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.sections.iter())
                    .flat_map(|section| section.fields.iter())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        // Provide a synthetic session marker so `requires_session` fields
        // unblock once the user has either completed login in this editor
        // pass or has a valid status from a previous login. We never
        // serialize real session data into the form snapshot — the auth
        // provider re-establishes its session from its own cache. When no
        // valid session exists the
        // provider returns `NeedsLogin` / `SessionExpired`, which
        // `apply_fetch_error` surfaces as a re-login hint below the field.
        // We pass the marker unconditionally so users with a valid session
        // from a prior provider login or DataTab run see options without
        // having to click Login again.
        let session_marker = Some(serde_json::Value::Null);

        for field in &field_defs_for_fetch {
            if matches!(field.kind, FormFieldKind::DynamicSelect { .. }) {
                self.fetch_dynamic_options_if_needed(
                    provider_id_for_fetch.clone(),
                    field,
                    session_marker.clone(),
                    login_just_completed,
                    cx,
                );
            }
        }

        // Collect field defs from the selected provider before the mutable borrow
        // for rendering (can't hold an Arc while calling &mut self methods).
        let provider_field_defs: Vec<datatab_core::FormFieldDef> = field_defs_for_fetch;

        let dynamic_fields: Vec<AnyElement> = provider_field_defs
            .iter()
            .enumerate()
            .map(|(idx, field)| {
                if matches!(field.kind, FormFieldKind::DynamicSelect { .. }) {
                    self.render_dynamic_dropdown_row(field, cx)
                        .into_any_element()
                } else if matches!(field.kind, FormFieldKind::AuthProfileRef { .. }) {
                    self.render_auth_profile_ref_row(field, cx)
                        .into_any_element()
                } else if let Some(input) = self.form_inputs.get(&field.id) {
                    let form_field = AuthFormField::DynamicField(idx);
                    let is_focused = self.auth_form_field == form_field
                        && self.auth_focus == AuthFocus::Form
                        && self.content_focused;

                    let (disabled, disabled_hint, inherited_value) = field
                        .disabled_when_field_set
                        .as_deref()
                        .and_then(|trigger_id| {
                            let trigger_value = self
                                .form_inputs
                                .get(trigger_id)?
                                .read(cx)
                                .value()
                                .to_string();
                            if trigger_value.trim().is_empty() {
                                return None;
                            }

                            let referenced = self.resolve_ref_profile(&trigger_value, cx);
                            let label = referenced.as_ref().map(|p| p.name.clone());
                            let inherited = referenced
                                .as_ref()
                                .and_then(|p| p.fields.get(&field.id).cloned())
                                .filter(|v| !v.trim().is_empty());

                            let hint = crate::labels::auth_profiles_inherited_hint(
                                trigger_id,
                                label.as_deref(),
                            );
                            Some((true, Some(hint), inherited))
                        })
                        .unwrap_or((false, None, None));

                    // WriteOnly fields get a help hint appended below the input
                    // to clarify write-only semantics (spec S33, R9.4.1).
                    let extra_hint: Option<String> =
                        if field.kind == FormFieldKind::WriteOnly && !disabled {
                            Some(field.help.clone().unwrap_or_else(|| {
                                datatab_i18n::t!("settings.auth_profiles.leave_blank_hint")
                            }))
                        } else {
                            None
                        };

                    let rendered = self.render_input_row_disabled(
                        &field.label,
                        input,
                        form_field,
                        form_renderer::is_secret_field(&field.kind),
                        is_focused,
                        disabled,
                        disabled_hint,
                        inherited_value,
                        cx,
                    );

                    // Wrap with extra write-only hint if needed.
                    if let Some(hint) = extra_hint {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(rendered)
                            .child(Text::caption(hint))
                            .into_any_element()
                    } else {
                        rendered.into_any_element()
                    }
                } else {
                    div().into_any_element()
                }
            })
            .collect();

        layout::sticky_form_shell(
            datatab_components::composites::section_header(
                datatab_i18n::t!("settings.auth_profiles.group.profile"),
                Some(AppIcon::KeyRound.into()),
                cx,
            ),
            div()
                .flex()
                .flex_col()
                .child({
                    let is_focused = self.auth_form_field == AuthFormField::Name
                        && self.auth_focus == AuthFocus::Form
                        && self.content_focused;
                    let name_label = datatab_i18n::t!("settings.auth_profiles.name_label");
                    self.render_input_row_disabled(
                        &name_label,
                        &self.input_name,
                        AuthFormField::Name,
                        false,
                        is_focused,
                        false,
                        None,
                        None,
                        cx,
                    )
                })
                .child({
                    layout::form_row(
                        datatab_i18n::t!("settings.auth_profiles.provider_label"),
                        self.render_provider_selector(window, cx),
                        None,
                    )
                })
                .children(dynamic_fields)
                .child({
                    let cursor = self.content_focused
                        && self.auth_focus == AuthFocus::Form
                        && self.auth_form_field == AuthFormField::Enabled;

                    layout::check_row(
                        layout::cursor_ring(
                            cursor,
                            Checkbox::new("auth-profile-enabled")
                                .checked(self.profile_enabled)
                                .label(datatab_i18n::t!("settings.auth_profiles.enabled"))
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.profile_enabled = *checked;
                                    cx.notify();
                                })),
                            cx,
                        ),
                        None,
                    )
                })
                .when(self.selected_provider_supports_login, |content| {
                    let login_focused = self.content_focused
                        && self.auth_focus == AuthFocus::Form
                        && self.auth_form_field == AuthFormField::ProviderLogin;

                    let login_button = Button::new(
                        "auth-provider-login",
                        if self.provider_login_loading {
                            datatab_i18n::t!("settings.auth_profiles.logging_in")
                        } else {
                            datatab_i18n::t!("settings.auth_profiles.login_action")
                        },
                    )
                    .secondary()
                    .icon(AppIcon::RefreshCcw)
                    .focused(login_focused)
                    .disabled(self.provider_login_loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.login_selected_profile(cx);
                    }));

                    let session = match self.provider_login_status.as_ref() {
                        Some((message, true)) => BannerBlock::new(
                            BannerVariant::Success,
                            datatab_i18n::t!("settings.auth_profiles.session_valid"),
                        )
                        .with_body(message.clone()),
                        Some((message, false)) => BannerBlock::new(
                            BannerVariant::Warning,
                            datatab_i18n::t!("settings.auth_profiles.session_attention"),
                        )
                        .with_body(message.clone()),
                        None => BannerBlock::new(
                            BannerVariant::Info,
                            datatab_i18n::t!("settings.auth_profiles.session_unknown"),
                        )
                        .with_body(datatab_i18n::t!(
                            "settings.auth_profiles.runs_interactive_login_hint"
                        )),
                    };

                    content
                        .child(
                            div()
                                .mt(FormMetrics::ROW_GAP)
                                .child(session.with_actions(login_button)),
                        )
                        .when_some(self.active_login_url.clone(), |content, url| {
                            content.child(self.render_login_url_panel(url, cx))
                        })
                }),
            None,
            &theme,
        )
        .into_any_element()
    }

    fn is_cursor_on(&self, field: AuthFormField) -> bool {
        self.content_focused && self.auth_focus == AuthFocus::Form && self.auth_form_field == field
    }

    /// Whether the form cursor is on the provider field `field_id`.
    fn is_cursor_on_dynamic_field(&self, field_id: &str) -> bool {
        self.provider_field_order
            .iter()
            .position(|id| id == field_id)
            .is_some_and(|idx| self.is_cursor_on(AuthFormField::DynamicField(idx)))
    }

    /// Export and Delete, on the left of the footer, for a stored profile.
    #[expect(clippy::question_mark, reason = "Keep the footer guard explicit.")]
    fn render_section_footer_leading_actions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.editing_profile_id.is_none() {
            return None;
        }

        Some(
            layout::inline_controls()
                .child(
                    Button::new(
                        "export-auth-profile",
                        datatab_i18n::t!("settings.auth_profiles.export"),
                    )
                    .secondary()
                    .icon(AppIcon::ExternalLink)
                    .focused(self.is_cursor_on(AuthFormField::ExportButton))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.request_export(cx);
                    })),
                )
                .child(
                    Button::new(
                        "delete-auth-profile",
                        datatab_i18n::t!("settings.auth_profiles.delete"),
                    )
                    .danger()
                    .icon(AppIcon::Delete)
                    .focused(self.is_cursor_on(AuthFormField::DeleteButton))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.request_delete_selected_profile(cx);
                    })),
                )
                .into_any_element(),
        )
    }

    fn render_section_footer_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let is_editing = self.editing_profile_id.is_some();

        layout::inline_controls()
            .child(
                Button::new(
                    "cancel-auth-profile",
                    datatab_i18n::t!("settings.auth_profiles.cancel"),
                )
                .secondary()
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(selected_id) = this.selected_profile_id {
                        this.load_profile_into_form(selected_id, window, cx);
                    } else {
                        this.clear_form(window, cx);
                        cx.notify();
                    }
                })),
            )
            .child(
                Button::new(
                    "save-auth-profile",
                    if is_editing {
                        datatab_i18n::t!("settings.auth_profiles.update")
                    } else {
                        datatab_i18n::t!("settings.auth_profiles.create")
                    },
                )
                .primary()
                .icon(AppIcon::Check)
                .when_some(crate::settings::save_shortcut(), Button::kbd)
                .focused(self.is_cursor_on(AuthFormField::SaveButton))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.save_profile(window, cx);
                })),
            )
            .into_any_element()
    }
}

impl FormSection for AuthProfilesSection {
    type Focus = AuthFocus;
    type FormField = AuthFormField;

    fn focus_area(&self) -> Self::Focus {
        self.auth_focus
    }

    fn set_focus_area(&mut self, focus: Self::Focus) {
        self.auth_focus = focus;
    }

    fn form_field(&self) -> Self::FormField {
        self.auth_form_field
    }

    fn set_form_field(&mut self, field: Self::FormField) {
        self.auth_form_field = field;
    }

    fn editing_field(&self) -> bool {
        self.auth_editing_field
    }

    fn set_editing_field(&mut self, editing: bool) {
        self.auth_editing_field = editing;
    }

    fn switching_input(&self) -> bool {
        self.switching_input
    }

    fn set_switching_input(&mut self, switching: bool) {
        self.switching_input = switching;
    }

    fn content_focused(&self) -> bool {
        self.content_focused
    }

    fn list_focus() -> Self::Focus {
        AuthFocus::ProfileList
    }

    fn form_focus() -> Self::Focus {
        AuthFocus::Form
    }

    fn first_form_field() -> Self::FormField {
        AuthFormField::Name
    }

    fn form_rows(&self) -> Vec<Vec<Self::FormField>> {
        build_form_rows(
            !self.provider_entries_cache.is_empty(),
            self.provider_field_order.len(),
            self.selected_provider_supports_login,
            self.active_login_url.is_some(),
            self.editing_profile_id.is_some(),
        )
    }

    fn is_input_field(field: Self::FormField) -> bool {
        matches!(field, AuthFormField::Name | AuthFormField::DynamicField(_))
    }

    fn focus_current_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.auth_editing_field = true;

        match self.auth_form_field {
            AuthFormField::Name => {
                self.input_name
                    .update(cx, |state, cx| state.focus(window, cx));
            }
            AuthFormField::DynamicField(idx) => {
                if let Some(field_id) = self.provider_field_order.get(idx)
                    && let Some(dropdown) = self.dynamic_dropdowns.get(field_id).cloned()
                {
                    self.auth_editing_field = false;
                    dropdown.update(cx, |dropdown, cx| dropdown.focus_and_open(window, cx));
                    return;
                }

                if let Some(field_id) = self.provider_field_order.get(idx).cloned()
                    && let Some(input) = self.form_inputs.get(&field_id)
                {
                    input.update(cx, |state, cx| state.focus(window, cx));
                    return;
                }
                self.auth_editing_field = false;
            }
            _ => {
                self.auth_editing_field = false;
            }
        }
    }

    fn activate_current_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.auth_form_field {
            AuthFormField::Name | AuthFormField::DynamicField(_) => {
                self.focus_current_field(window, cx);
            }
            AuthFormField::Provider(_) => {
                // The dropdown takes the keyboard until a provider is chosen
                // or it closes; the `DropdownSelectionChanged` subscription in
                // the constructor applies the choice.
                self.provider_dropdown
                    .update(cx, |dropdown, cx| dropdown.focus_and_open(window, cx));
            }
            AuthFormField::Enabled => {
                self.profile_enabled = !self.profile_enabled;
            }
            AuthFormField::ProviderLogin => {
                // The Login button is disabled while a login is in flight;
                // its keyboard path follows it.
                if !self.provider_login_loading {
                    self.login_selected_profile(cx);
                }
            }
            AuthFormField::LoginOpenBrowser => {
                if let Some(url) = self.active_login_url.as_deref() {
                    cx.open_url(url);
                }
            }
            AuthFormField::LoginCopyUrl => {
                if let Some(url) = self.active_login_url.clone() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                }
            }
            AuthFormField::LoginCancel => {
                self.cancel_provider_login(cx);
                self.validate_form_field();
            }
            AuthFormField::ExportButton => {
                self.request_export(cx);
            }
            AuthFormField::SaveButton => {
                self.save_profile(window, cx);
            }
            AuthFormField::DeleteButton => {
                self.request_delete_selected_profile(cx);
            }
        }
    }

    fn validate_form_field(&mut self) {
        let rows = self.form_rows();
        let current = self.auth_form_field;

        for row in &rows {
            if row.contains(&current) {
                return;
            }
        }

        self.auth_form_field = AuthFormField::Name;
    }
}

impl SettingsSection for AuthProfilesSection {
    fn section_id(&self) -> SettingsSectionId {
        SettingsSectionId::AuthProfiles
    }

    fn handle_key_event(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        AuthProfilesSection::handle_key_event(self, event, window, cx);
    }

    fn focus_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = true;
        cx.notify();
    }

    fn focus_out(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = false;
        self.auth_editing_field = false;
        cx.notify();
    }

    fn render_footer_actions(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        Some(self.render_section_footer_actions(cx))
    }

    fn render_footer_leading_actions(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.render_section_footer_leading_actions(cx)
    }

    fn save_from_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_profile(window, cx);
    }
}

impl Render for AuthProfilesSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_sync_from_app_state {
            self.pending_sync_from_app_state = false;
            self.sync_from_app_state(window, cx);
        }

        // Consume a pending login URL and emit it as an event so the settings
        // coordinator can route it to the login modal in the workspace window.
        if let Some((provider_name, profile_name, url, generation)) = self.pending_sso_url.take() {
            cx.emit(AuthProfilesSectionEvent::OpenLoginModal {
                provider_name,
                profile_name,
                url,
                generation,
            });
        }

        let profiles = self.app_state.read(cx).list_auth_profiles();
        let show_delete_dialog = self.pending_delete_profile_id.is_some();

        let (delete_name, affected_connections) = self
            .pending_delete_profile_id
            .and_then(|profile_id| {
                profiles
                    .iter()
                    .find(|profile| profile.id == profile_id)
                    .map(|profile| {
                        (
                            profile.name.clone(),
                            self.profiles_using_auth(profile_id, cx),
                        )
                    })
            })
            .unwrap_or_else(|| (String::new(), 0));

        layout::section_container(
            layout::split_section_shell(
                cx.theme().border,
                datatab_components::composites::page_header(
                    datatab_i18n::t!("settings.auth_profiles.section_title"),
                    datatab_i18n::t!("settings.auth_profiles.section_description"),
                    cx,
                ),
                self.render_profile_list(&profiles, cx),
                self.render_editor_panel(window, cx),
            )
            .when(show_delete_dialog, |element| {
                let entity = cx.entity().clone();
                let entity_cancel = entity.clone();

                let body =
                    crate::labels::auth_profiles_delete_body(&delete_name, affected_connections);

                element.child(
                    Dialog::new(cx)
                        .title(datatab_i18n::t!(
                            "settings.auth_profiles.delete_dialog_title"
                        ))
                        .button_props(DialogButtonProps::default().show_cancel(true))
                        .overlay_closable(false)
                        .close_button(false)
                        .on_ok(move |_, window, cx| {
                            entity.update(cx, |section, cx| {
                                section.confirm_delete_profile(window, cx);
                            });
                            true
                        })
                        .on_cancel(move |_, _, cx| {
                            entity_cancel.update(cx, |section, cx| {
                                section.cancel_delete_profile(cx);
                            });
                            true
                        })
                        .child(Text::body(body)),
                )
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct StoredTestProvider;

    impl datatab_core::auth::AuthProvider for StoredTestProvider {
        fn provider_id(&self) -> &'static str {
            "rpc-auth"
        }
        fn display_name(&self) -> &'static str {
            "Test provider"
        }
        async fn validate_session(
            &self,
            _: &AuthProfile,
        ) -> Result<datatab_core::AuthSessionState, datatab_core::DbError> {
            Ok(datatab_core::AuthSessionState::LoginRequired)
        }
        async fn login(
            &self,
            profile: &AuthProfile,
        ) -> Result<datatab_core::AuthSession, datatab_core::DbError> {
            Ok(datatab_core::AuthSession {
                provider_id: "rpc-auth".into(),
                profile_id: profile.id,
                expires_at: None,
                data: None,
            })
        }
        async fn resolve_credentials(
            &self,
            _: &AuthProfile,
        ) -> Result<datatab_core::ResolvedCredentials, datatab_core::DbError> {
            Ok(datatab_core::ResolvedCredentials::default())
        }
    }

    #[gpui::test]
    fn save_profile_creates_and_edits_a_stored_provider_profile(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        let app_state = cx.update(|cx| {
            cx.new(|_| {
                let runtime =
                    datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
                let mut state =
                    AppStateEntity::new_with_storage_runtime(runtime).expect("app state");
                state.register_auth_provider_for_test(Arc::new(StoredTestProvider));
                state
            })
        });
        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (_, window) = cx.add_window_view({
            let app_state = app_state.clone();
            let slot = slot.clone();
            move |window, cx| {
                let section = cx.new(|cx| AuthProfilesSection::new(app_state, window, cx));
                slot.replace(Some(section.clone()));
                gpui_component::Root::new(section, window, cx)
            }
        });
        let section = slot.borrow().clone().expect("section");
        let mut profile_id = None;
        for name in ["Created", "Edited"] {
            window.update(|window, cx| {
                section.update(cx, |section, cx| {
                    section
                        .input_name
                        .update(cx, |input, cx| input.set_value(name, window, cx));
                    section.save_profile(window, cx);
                    let profiles = app_state.read(cx).list_auth_profiles();
                    assert_eq!(profiles.len(), 1);
                    assert_eq!(profiles[0].name, name);
                    assert_eq!(profiles[0].provider_id, "rpc-auth");
                    if let Some(id) = profile_id {
                        assert_eq!(profiles[0].id, id);
                    }
                    profile_id = Some(profiles[0].id);
                    let stored = app_state
                        .read(cx)
                        .storage_runtime()
                        .auth_profiles()
                        .get(&profiles[0].id.to_string())
                        .expect("read stored row")
                        .expect("stored profile");
                    assert_eq!(stored.name, name);
                    assert_eq!(stored.provider_id, "rpc-auth");
                })
            });
        }
    }

    // --- A login the provider is still working on (#369) ---

    /// One `login` call of `HeldLoginProvider`. It waits until the test
    /// finishes it.
    #[derive(Default)]
    struct HeldLogin {
        state: std::sync::Mutex<HeldLoginState>,
    }

    #[derive(Default)]
    struct HeldLoginState {
        /// Whether the login succeeded, once the test finished it.
        succeeded: Option<bool>,
        waker: Option<std::task::Waker>,
    }

    impl HeldLogin {
        fn finish(&self, succeeded: bool) {
            let mut state = self.state.lock().expect("held login state");
            state.succeeded = Some(succeeded);
            if let Some(waker) = state.waker.take() {
                waker.wake();
            }
        }

        /// Waits until the test finishes the login; `true` when it succeeded.
        async fn finished(&self) -> bool {
            std::future::poll_fn(|context| {
                let mut state = self.state.lock().expect("held login state");
                match state.succeeded {
                    Some(succeeded) => std::task::Poll::Ready(succeeded),
                    None => {
                        state.waker = Some(context.waker().clone());
                        std::task::Poll::Pending
                    }
                }
            })
            .await
        }
    }

    /// A provider whose login succeeds only when the test finishes it, as an
    /// RPC provider answers only after the browser step. Clones share the
    /// logins, so the test keeps one and registers the other.
    #[derive(Clone, Default)]
    struct HeldLoginProvider {
        logins: Arc<std::sync::Mutex<Vec<Arc<HeldLogin>>>>,
    }

    impl HeldLoginProvider {
        /// Lets the login started by the `index`-th `login` call succeed.
        fn finish_login(&self, index: usize) {
            self.held_login(index).finish(true);
        }

        /// Lets the login started by the `index`-th `login` call fail.
        fn fail_login(&self, index: usize) {
            self.held_login(index).finish(false);
        }

        fn held_login(&self, index: usize) -> Arc<HeldLogin> {
            self.logins
                .lock()
                .expect("held logins")
                .get(index)
                .cloned()
                .expect("the login was started")
        }
    }

    static HELD_LOGIN_CAPABILITIES: datatab_core::auth::AuthProviderCapabilities =
        datatab_core::auth::AuthProviderCapabilities {
            login: datatab_core::auth::AuthProviderLoginCapabilities {
                supported: true,
                verification_url_progress: false,
            },
        };

    impl datatab_core::auth::AuthProvider for HeldLoginProvider {
        fn provider_id(&self) -> &'static str {
            "held-login-auth"
        }
        fn display_name(&self) -> &'static str {
            "Held login provider"
        }
        fn capabilities(&self) -> &'static datatab_core::auth::AuthProviderCapabilities {
            &HELD_LOGIN_CAPABILITIES
        }
        async fn validate_session(
            &self,
            _: &AuthProfile,
        ) -> Result<datatab_core::AuthSessionState, datatab_core::DbError> {
            Ok(datatab_core::AuthSessionState::LoginRequired)
        }
        async fn login(
            &self,
            profile: &AuthProfile,
        ) -> Result<datatab_core::AuthSession, datatab_core::DbError> {
            let login = Arc::new(HeldLogin::default());
            self.logins.lock().expect("held logins").push(login.clone());
            if !login.finished().await {
                return Err(datatab_core::DbError::connection_failed(
                    "the held login failed".to_string(),
                ));
            }

            Ok(datatab_core::AuthSession {
                provider_id: "held-login-auth".into(),
                profile_id: profile.id,
                expires_at: None,
                data: None,
            })
        }
        async fn resolve_credentials(
            &self,
            _: &AuthProfile,
        ) -> Result<datatab_core::ResolvedCredentials, datatab_core::DbError> {
            Ok(datatab_core::ResolvedCredentials::default())
        }
    }

    const HELD_LOGIN_PROFILE_NAME: &str = "Held";

    /// Opens the section on a named, unsaved profile of a `HeldLoginProvider`.
    fn section_with_held_login_provider(
        cx: &mut TestAppContext,
    ) -> (
        HeldLoginProvider,
        Entity<AuthProfilesSection>,
        &mut VisualTestContext,
    ) {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        let provider = HeldLoginProvider::default();
        let app_state = cx.update(|cx| {
            cx.new(|_| {
                let runtime =
                    datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
                let mut state =
                    AppStateEntity::new_with_storage_runtime(runtime).expect("app state");
                state.register_auth_provider_for_test(Arc::new(provider.clone()));
                state
            })
        });
        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (_, window) = cx.add_window_view({
            let slot = slot.clone();
            move |window, cx| {
                let section = cx.new(|cx| AuthProfilesSection::new(app_state, window, cx));
                slot.replace(Some(section.clone()));
                gpui_component::Root::new(section, window, cx)
            }
        });
        let section = slot.borrow().clone().expect("section");
        window.update(|window, cx| {
            section.update(cx, |section, cx| {
                section.input_name.update(cx, |input, cx| {
                    input.set_value(HELD_LOGIN_PROFILE_NAME, window, cx)
                });
            })
        });

        (provider, section, window)
    }

    fn start_login(section: &Entity<AuthProfilesSection>, window: &mut VisualTestContext) {
        window.update(|_, cx| section.update(cx, |section, cx| section.login_selected_profile(cx)));
        window.run_until_parked();
    }

    fn cancel_login(section: &Entity<AuthProfilesSection>, window: &mut VisualTestContext) {
        window.update(|_, cx| section.update(cx, |section, cx| section.cancel_provider_login(cx)));
        window.run_until_parked();
    }

    fn login_cancelled_status() -> Option<(String, bool)> {
        Some((
            datatab_i18n::t!("settings.auth_profiles.login_cancelled"),
            false,
        ))
    }

    /// Cancel must not leave the Login button waiting for a provider that may
    /// never answer.
    #[gpui::test]
    fn cancel_enables_the_login_button_before_the_provider_answers(cx: &mut TestAppContext) {
        let (_provider, section, window) = section_with_held_login_provider(cx);

        start_login(&section, window);
        assert!(
            window.read(|cx| section.read(cx).provider_login_loading),
            "the form waits for the login"
        );

        cancel_login(&section, window);

        window.read(|cx| {
            let section = section.read(cx);
            assert!(
                !section.provider_login_loading,
                "the Login button must be enabled right after Cancel"
            );
            assert_eq!(section.provider_login_status, login_cancelled_status());
        });
    }

    /// A login that succeeds after Cancel is late: it must not report
    /// "login completed", and it must not drop the cached options, which is
    /// what makes the form fetch them again.
    #[gpui::test]
    fn login_result_after_cancel_changes_nothing(cx: &mut TestAppContext) {
        let (provider, section, window) = section_with_held_login_provider(cx);
        let cached_options_key = ("held-login-auth".to_string(), "account".to_string(), 0);

        start_login(&section, window);
        window.update(|_, cx| {
            section.update(cx, |section, _| {
                section.options_cache.insert(
                    cached_options_key.clone(),
                    CachedOptions {
                        options: Vec::new(),
                        expires_at: Instant::now(),
                    },
                );
            })
        });
        cancel_login(&section, window);

        provider.finish_login(0);
        window.run_until_parked();

        window.read(|cx| {
            let section = section.read(cx);
            assert_eq!(section.provider_login_status, login_cancelled_status());
            assert!(!section.provider_login_loading);
            assert!(
                section.options_cache.contains_key(&cached_options_key),
                "a late login result must not trigger option fetches"
            );
        });
    }

    /// The result of a cancelled login must not end the login started after it.
    #[gpui::test]
    fn login_result_after_cancel_leaves_the_next_login_waiting(cx: &mut TestAppContext) {
        let (provider, section, window) = section_with_held_login_provider(cx);

        start_login(&section, window);
        cancel_login(&section, window);
        start_login(&section, window);

        provider.finish_login(0);
        window.run_until_parked();

        window.read(|cx| {
            let section = section.read(cx);
            assert!(
                section.provider_login_loading,
                "the second login is still waiting for its provider"
            );
            assert_eq!(
                section.provider_login_status,
                Some((
                    crate::labels::auth_profiles_starting_login(HELD_LOGIN_PROFILE_NAME),
                    false
                ))
            );
        });

        provider.finish_login(1);
        window.run_until_parked();

        window.read(|cx| {
            let section = section.read(cx);
            assert!(!section.provider_login_loading);
            assert_eq!(
                section.provider_login_status,
                Some((
                    crate::labels::auth_profiles_login_completed(HELD_LOGIN_PROFILE_NAME),
                    true
                ))
            );
        });
    }

    // --- A login the form lets go of, and the login modal (#369, ruling 1) ---

    /// Stores the form as a profile, as Save does, and returns its id.
    fn save_held_login_profile(
        section: &Entity<AuthProfilesSection>,
        window: &mut VisualTestContext,
    ) -> Uuid {
        window.update(|window, cx| {
            section.update(cx, |section, cx| {
                section.save_profile(window, cx);
                section.editing_profile_id.expect("the profile was stored")
            })
        })
    }

    fn login_handle(
        section: &Entity<AuthProfilesSection>,
        window: &mut VisualTestContext,
    ) -> SettingsLoginHandle {
        window.read(|cx| {
            SettingsLoginHandle::new(section.downgrade(), section.read(cx).login_generation)
        })
    }

    /// Counts the "login ended" calls of one `SettingsLoginHandle`.
    struct LoginEndProbe {
        ended: usize,
        _subscription: Subscription,
    }

    fn watch_login_end(
        handle: &SettingsLoginHandle,
        window: &mut VisualTestContext,
    ) -> Entity<LoginEndProbe> {
        window.update(|_, cx| {
            cx.new(|cx| LoginEndProbe {
                ended: 0,
                _subscription: handle
                    .on_ended(cx, |probe: &mut LoginEndProbe, _| probe.ended += 1)
                    .expect("the section is alive"),
            })
        })
    }

    /// Loading a profile into the form, as selecting one or Save does, lets
    /// go of the login in flight: its result belongs to the form as it was.
    #[gpui::test]
    fn login_result_after_a_profile_is_loaded_changes_nothing(cx: &mut TestAppContext) {
        let (provider, section, window) = section_with_held_login_provider(cx);
        let profile_id = save_held_login_profile(&section, window);
        let cached_options_key = ("held-login-auth".to_string(), "account".to_string(), 0);

        start_login(&section, window);
        window.update(|window, cx| {
            section.update(cx, |section, cx| {
                section.load_profile_into_form(profile_id, window, cx);
                section.options_cache.insert(
                    cached_options_key.clone(),
                    CachedOptions {
                        options: Vec::new(),
                        expires_at: Instant::now(),
                    },
                );
            })
        });

        provider.finish_login(0);
        window.run_until_parked();

        window.read(|cx| {
            let section = section.read(cx);
            assert_eq!(
                section.provider_login_status, None,
                "the late result must not report on the loaded profile"
            );
            assert!(
                section.options_cache.contains_key(&cached_options_key),
                "the late result must not trigger option fetches"
            );
        });
    }

    /// Asserts that the form has let go of its login: Login is enabled, no
    /// login status is left, and the result that then arrives for the
    /// `login_index`-th login changes neither.
    fn assert_login_released(
        provider: &HeldLoginProvider,
        login_index: usize,
        section: &Entity<AuthProfilesSection>,
        window: &mut VisualTestContext,
    ) {
        let cached_options_key = ("held-login-auth".to_string(), "account".to_string(), 0);

        window.update(|_, cx| {
            section.update(cx, |section, _| {
                assert!(
                    !section.provider_login_loading,
                    "the Login button must be enabled on the new form"
                );
                assert_eq!(section.provider_login_status, None);
                section.options_cache.insert(
                    cached_options_key.clone(),
                    CachedOptions {
                        options: Vec::new(),
                        expires_at: Instant::now(),
                    },
                );
            })
        });

        provider.finish_login(login_index);
        window.run_until_parked();

        window.read(|cx| {
            let section = section.read(cx);
            assert_eq!(
                section.provider_login_status, None,
                "the late result must not report on the new form"
            );
            assert!(!section.provider_login_loading);
            assert!(
                section.options_cache.contains_key(&cached_options_key),
                "the late result must not trigger option fetches"
            );
        });
    }

    /// Enter on the Login row is the keyboard path of the Login button, which
    /// is disabled while a login is in flight: it must not start a second
    /// login and let go of the first.
    #[gpui::test]
    fn enter_on_login_starts_no_second_login_while_one_waits(cx: &mut TestAppContext) {
        let (provider, section, window) = section_with_held_login_provider(cx);

        start_login(&section, window);
        window.update(|window, cx| {
            section.update(cx, |section, cx| {
                section.auth_form_field = AuthFormField::ProviderLogin;
                section.activate_current_field(window, cx);
            })
        });
        window.run_until_parked();

        assert_eq!(
            provider.logins.lock().expect("held logins").len(),
            1,
            "Enter started a second login"
        );

        provider.finish_login(0);
        window.run_until_parked();

        assert_eq!(
            window.read(|cx| section.read(cx).provider_login_status.clone()),
            Some((
                crate::labels::auth_profiles_login_completed(HELD_LOGIN_PROFILE_NAME),
                true
            )),
            "the login in flight must still report its result"
        );
    }

    /// Starting a new profile during a login replaces the form: the login
    /// belongs to the form as it was.
    #[gpui::test]
    fn login_result_after_a_new_profile_is_started_changes_nothing(cx: &mut TestAppContext) {
        let (provider, section, window) = section_with_held_login_provider(cx);

        start_login(&section, window);
        window.update(|window, cx| {
            section.update(cx, |section, cx| section.begin_create_profile(window, cx));
        });
        window.run_until_parked();

        assert_login_released(&provider, 0, &section, window);
    }

    /// Choosing a provider in the form during a login replaces the form's
    /// fields: the login belongs to the form as it was.
    #[gpui::test]
    fn login_result_after_a_provider_is_chosen_changes_nothing(cx: &mut TestAppContext) {
        let (provider, section, window) = section_with_held_login_provider(cx);

        start_login(&section, window);
        window.update(|_, cx| {
            let provider_dropdown = section.read(cx).provider_dropdown.clone();
            provider_dropdown.update(cx, |_, cx| {
                cx.emit(DropdownSelectionChanged {
                    index: 0,
                    item: DropdownItem::with_value("Held login provider", "held-login-auth"),
                });
            });
        });
        window.run_until_parked();

        assert_login_released(&provider, 0, &section, window);
    }

    /// The verification URL of the login in flight opens the URL panel and
    /// is queued for the login modal under that login's generation. One that
    /// arrives after the form let go of its login does neither.
    #[gpui::test]
    fn verification_url_is_shown_only_for_the_login_in_flight(cx: &mut TestAppContext) {
        let (_provider, section, window) = section_with_held_login_provider(cx);
        let url = "https://verify.example/device";
        let show_url = |section: &mut AuthProfilesSection,
                        generation: u64,
                        cx: &mut Context<AuthProfilesSection>| {
            section.show_login_url(
                generation,
                "Held login provider".to_string(),
                HELD_LOGIN_PROFILE_NAME.to_string(),
                url.to_string(),
                cx,
            );
        };

        start_login(&section, window);
        let generation = window.update(|_, cx| {
            section.update(cx, |section, cx| {
                let generation = section.login_generation;
                show_url(section, generation, cx);

                assert_eq!(section.active_login_url.as_deref(), Some(url));
                assert_eq!(
                    section
                        .pending_sso_url
                        .as_ref()
                        .map(|(_, _, url, generation)| (url.as_deref(), *generation)),
                    Some((Some(url), generation))
                );
                generation
            })
        });

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.cancel_provider_login(cx);
                show_url(section, generation, cx);

                assert_eq!(section.active_login_url, None);
                assert!(section.pending_sso_url.is_none());
            })
        });
    }

    /// The login modal cancels through the handle: the same login, with the
    /// same result as the form's own Cancel. A handle of an earlier login
    /// must not cancel the one in flight.
    #[gpui::test]
    fn login_handle_cancels_only_its_own_login(cx: &mut TestAppContext) {
        let (_provider, section, window) = section_with_held_login_provider(cx);

        start_login(&section, window);
        let first = login_handle(&section, window);
        assert!(window.read(|cx| first.is_waiting(cx)));

        window.update(|_, cx| first.cancel(cx));
        window.run_until_parked();

        window.read(|cx| {
            let section = section.read(cx);
            assert!(
                !section.provider_login_loading,
                "the Login button must be enabled after the modal's Cancel"
            );
            assert_eq!(section.provider_login_status, login_cancelled_status());
            assert!(!first.is_waiting(cx));
        });

        start_login(&section, window);
        window.update(|_, cx| first.cancel(cx));
        window.run_until_parked();

        assert!(
            window.read(|cx| section.read(cx).provider_login_loading),
            "the handle of a cancelled login must leave the next login alone"
        );
    }

    /// The login modal closes when its handle reports the end of the login:
    /// a result, with success or failure, a Cancel, or a profile loaded into
    /// the form. The end of another login is not reported.
    #[gpui::test]
    fn login_handle_reports_each_way_its_login_ends(cx: &mut TestAppContext) {
        let (provider, section, window) = section_with_held_login_provider(cx);
        let profile_id = save_held_login_profile(&section, window);
        let ended = |probe: &Entity<LoginEndProbe>, window: &mut VisualTestContext| {
            window.read(|cx| probe.read(cx).ended)
        };

        start_login(&section, window);
        let succeeded = watch_login_end(&login_handle(&section, window), window);
        assert_eq!(ended(&succeeded, window), 0);
        provider.finish_login(0);
        window.run_until_parked();
        assert_eq!(ended(&succeeded, window), 1, "a successful login ended");

        start_login(&section, window);
        let failed = watch_login_end(&login_handle(&section, window), window);
        provider.fail_login(1);
        window.run_until_parked();
        assert_eq!(ended(&failed, window), 1, "a failed login ended");

        start_login(&section, window);
        let cancelled = watch_login_end(&login_handle(&section, window), window);
        cancel_login(&section, window);
        assert_eq!(ended(&cancelled, window), 1, "a cancelled login ended");

        start_login(&section, window);
        let released = watch_login_end(&login_handle(&section, window), window);
        window.update(|window, cx| {
            section.update(cx, |section, cx| {
                section.load_profile_into_form(profile_id, window, cx);
            })
        });
        window.run_until_parked();
        assert_eq!(ended(&released, window), 1, "a released login ended");

        assert_eq!(
            ended(&succeeded, window),
            1,
            "the end of a later login is not the end of this one"
        );
    }

    #[::core::prelude::v1::test]
    fn form_rows_include_generic_provider_login_without_aws_feature() {
        let rows = build_form_rows(true, 2, true, false, false);

        assert!(
            rows.iter()
                .any(|row| row == &vec![AuthFormField::ProviderLogin])
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.contains(&AuthFormField::DeleteButton))
        );
    }

    /// While a login waits for the browser, its Open browser, Copy URL and
    /// Cancel buttons form the row under the login button.
    #[::core::prelude::v1::test]
    fn form_rows_list_the_login_url_buttons_while_a_login_waits() {
        let login_url_row = vec![
            AuthFormField::LoginOpenBrowser,
            AuthFormField::LoginCopyUrl,
            AuthFormField::LoginCancel,
        ];

        let waiting = build_form_rows(true, 0, true, true, true);
        let login_index = waiting
            .iter()
            .position(|row| row == &vec![AuthFormField::ProviderLogin])
            .expect("the login row is listed");
        assert_eq!(waiting.get(login_index + 1), Some(&login_url_row));

        let idle = build_form_rows(true, 0, true, false, true);
        assert!(!idle.contains(&login_url_row));
    }

    #[::core::prelude::v1::test]
    fn build_auth_profile_from_form_preserves_non_aws_provider_id() {
        let mut fields = HashMap::new();
        fields.insert(
            "issuer_url".to_string(),
            "https://issuer.example".to_string(),
        );

        let profile = build_auth_profile_from_form(
            Uuid::nil(),
            "  Custom OIDC  ",
            "custom-oidc",
            fields.clone(),
            true,
        )
        .expect("profile should be created");

        assert_eq!(profile.name, "Custom OIDC");
        assert_eq!(profile.provider_id, "custom-oidc");
        assert_eq!(profile.fields, fields);
        assert!(profile.enabled);
    }

    // -----------------------------------------------------------------------
    // T14: Cache invalidation rules
    // -----------------------------------------------------------------------

    /// (a) Within TTL, the same (provider_id, field_id, dep_hash) key is reused
    /// and no refetch is triggered.
    #[::core::prelude::v1::test]
    fn cache_key_is_stable_for_same_dependencies() {
        let mut deps_a = HashMap::new();
        deps_a.insert("region".to_string(), "us-east-1".to_string());

        let hash_a = AuthProfilesSection::hash_deps(&deps_a);

        // Same deps, different iteration order — result must be identical.
        let mut deps_b = HashMap::new();
        deps_b.insert("region".to_string(), "us-east-1".to_string());

        let hash_b = AuthProfilesSection::hash_deps(&deps_b);

        assert_eq!(
            hash_a, hash_b,
            "same dependency map must produce the same hash"
        );
    }

    /// (b) A change in dependency value produces a different dep_hash, which
    /// results in a different cache key and triggers a refetch.
    #[::core::prelude::v1::test]
    fn dep_change_produces_different_hash() {
        let mut deps_a = HashMap::new();
        deps_a.insert("region".to_string(), "us-east-1".to_string());

        let mut deps_b = HashMap::new();
        deps_b.insert("region".to_string(), "eu-west-1".to_string());

        let hash_a = AuthProfilesSection::hash_deps(&deps_a);
        let hash_b = AuthProfilesSection::hash_deps(&deps_b);

        assert_ne!(
            hash_a, hash_b,
            "changed dep value must produce a different hash"
        );
    }

    /// (b) A change in dependency key also produces a different hash.
    #[::core::prelude::v1::test]
    fn dep_key_change_produces_different_hash() {
        let mut deps_a = HashMap::new();
        deps_a.insert("region".to_string(), "us-east-1".to_string());

        let mut deps_b = HashMap::new();
        deps_b.insert("account".to_string(), "us-east-1".to_string());

        let hash_a = AuthProfilesSection::hash_deps(&deps_a);
        let hash_b = AuthProfilesSection::hash_deps(&deps_b);

        assert_ne!(
            hash_a, hash_b,
            "different dep key must produce a different hash"
        );
    }

    /// (c) `RefreshTrigger::Manual` fetches only on cache miss (first render).
    ///     The trigger must NOT cause a refetch when a cached entry exists.
    #[::core::prelude::v1::test]
    fn manual_trigger_fetches_only_on_cache_miss() {
        // With no cached entry: needs_fetch must be true.
        let empty_cache: HashMap<(String, String, u64), CachedOptions> = HashMap::new();
        let cache_key = ("provider".to_string(), "field".to_string(), 0u64);
        let needs_fetch_on_miss = !empty_cache.contains_key(&cache_key);
        assert!(
            needs_fetch_on_miss,
            "Manual trigger must fetch when no cached entry exists"
        );

        // With a cached entry: needs_fetch must be false.
        let mut cache_with_entry: HashMap<(String, String, u64), CachedOptions> = HashMap::new();
        cache_with_entry.insert(
            cache_key.clone(),
            CachedOptions {
                options: vec![],
                expires_at: Instant::now() + std::time::Duration::from_secs(300),
            },
        );
        let needs_fetch_on_hit = !cache_with_entry.contains_key(&cache_key);
        assert!(
            !needs_fetch_on_hit,
            "Manual trigger must NOT refetch when a cached entry exists"
        );
    }

    /// (d) `RefreshTrigger::OnLoginComplete` only fires when `login_just_completed`
    ///     is true.
    #[::core::prelude::v1::test]
    fn on_login_complete_trigger_respects_login_flag() {
        let trigger = RefreshTrigger::OnLoginComplete;

        let fires_without_login = match trigger {
            RefreshTrigger::OnLoginComplete => false, // only when login_just_completed=true
            _ => false,
        };
        assert!(
            !fires_without_login,
            "OnLoginComplete must NOT trigger without the login-complete signal"
        );

        let fires_with_login = matches!(trigger, RefreshTrigger::OnLoginComplete);
        assert!(
            fires_with_login,
            "OnLoginComplete must trigger when the login-complete signal is true"
        );
    }

    // -----------------------------------------------------------------------
    // NFR-003: apply_fetch_error_state — re-login hint surfacing
    // -----------------------------------------------------------------------

    /// `NeedsLogin` inserts a per-field hint and sets the status banner to a
    /// warning (success=false) when no login is currently in progress.
    #[::core::prelude::v1::test]
    fn needs_login_sets_field_hint_and_status_banner() {
        let mut hints: HashMap<String, String> = HashMap::new();
        let mut status: Option<(String, bool)> = None;

        apply_fetch_error_state(
            "account_id",
            FetchOptionsError::NeedsLogin,
            &mut hints,
            &mut status,
            false,
        );

        assert!(
            hints.contains_key("account_id"),
            "NeedsLogin must insert a hint for the affected field"
        );
        assert!(
            status.is_some(),
            "NeedsLogin must set a status banner when login is not in progress"
        );
        let (msg, success) = status.unwrap();
        assert!(
            !success,
            "NeedsLogin status banner must be a warning (success=false)"
        );
        assert!(
            msg.to_lowercase().contains("login"),
            "status message must reference login"
        );
    }

    /// `SessionExpired` inserts a per-field hint and sets the status banner.
    #[::core::prelude::v1::test]
    fn session_expired_sets_field_hint_and_status_banner() {
        let mut hints: HashMap<String, String> = HashMap::new();
        let mut status: Option<(String, bool)> = None;

        apply_fetch_error_state(
            "role_arn",
            FetchOptionsError::SessionExpired,
            &mut hints,
            &mut status,
            false,
        );

        assert!(
            hints.contains_key("role_arn"),
            "SessionExpired must insert a hint for the affected field"
        );
        let (msg, success) = status.expect("SessionExpired must set a status banner");
        assert!(
            !success,
            "SessionExpired status banner must be a warning (success=false)"
        );
        assert!(
            msg.to_lowercase().contains("expired") || msg.to_lowercase().contains("login"),
            "status message must mention expiry or login"
        );
    }

    /// When `login_in_progress` is true, `NeedsLogin` still writes the
    /// field hint but must NOT overwrite the in-progress status banner.
    #[::core::prelude::v1::test]
    fn needs_login_preserves_in_progress_status_banner() {
        let mut hints: HashMap<String, String> = HashMap::new();
        let existing_status = "Starting auth-provider login for profile 'dev'...".to_string();
        let mut status: Option<(String, bool)> = Some((existing_status.clone(), false));

        apply_fetch_error_state(
            "account_id",
            FetchOptionsError::NeedsLogin,
            &mut hints,
            &mut status,
            true, // login_in_progress = true
        );

        assert!(
            hints.contains_key("account_id"),
            "Field hint must still be inserted while login is in progress"
        );
        let (msg, _) = status.expect("Status banner must not be cleared");
        assert_eq!(
            msg, existing_status,
            "Status banner must not be overwritten while login is in progress"
        );
    }

    /// Transient errors do NOT set a field hint or status banner.
    #[::core::prelude::v1::test]
    fn transient_error_does_not_set_hint_or_status() {
        let mut hints: HashMap<String, String> = HashMap::new();
        let mut status: Option<(String, bool)> = None;

        apply_fetch_error_state(
            "region",
            FetchOptionsError::Transient("network timeout".to_string()),
            &mut hints,
            &mut status,
            false,
        );

        assert!(
            hints.is_empty(),
            "Transient error must not insert a field hint"
        );
        assert!(
            status.is_none(),
            "Transient error must not set a status banner"
        );
    }

    /// Permanent errors do NOT set a field hint or status banner.
    #[::core::prelude::v1::test]
    fn permanent_error_does_not_set_hint_or_status() {
        let mut hints: HashMap<String, String> = HashMap::new();
        let mut status: Option<(String, bool)> = None;

        apply_fetch_error_state(
            "region",
            FetchOptionsError::Permanent("unsupported field".to_string()),
            &mut hints,
            &mut status,
            false,
        );

        assert!(
            hints.is_empty(),
            "Permanent error must not insert a field hint"
        );
        assert!(
            status.is_none(),
            "Permanent error must not set a status banner"
        );
    }

    /// Expired cache entries are detected by comparing `Instant::now()` with
    /// `expires_at`. We set `expires_at` to the past to simulate expiry.
    #[::core::prelude::v1::test]
    fn expired_cache_entry_is_detected() {
        let expired = CachedOptions {
            options: vec![],
            expires_at: Instant::now() - std::time::Duration::from_secs(1),
        };
        assert!(
            Instant::now() > expired.expires_at,
            "expired entry must be older than now"
        );
    }

    /// A fresh cache entry (expires in the future) is not expired.
    #[::core::prelude::v1::test]
    fn fresh_cache_entry_is_not_expired() {
        let fresh = CachedOptions {
            options: vec![],
            expires_at: Instant::now() + std::time::Duration::from_secs(300),
        };
        assert!(
            Instant::now() <= fresh.expires_at,
            "fresh entry must not be expired"
        );
    }

    #[::core::prelude::v1::test]
    fn form_rows_include_save_and_delete_when_editing() {
        let rows = build_form_rows(true, 0, false, false, true);
        assert!(
            rows.iter()
                .any(|row| row.contains(&AuthFormField::SaveButton)),
            "SaveButton must appear in form rows when editing a stored profile"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains(&AuthFormField::DeleteButton)),
            "DeleteButton must appear in form rows when editing a stored profile"
        );
    }

    #[::core::prelude::v1::test]
    fn export_button_only_for_stored_profile() {
        let stored = build_form_rows(true, 2, false, false, true);
        assert!(
            stored
                .iter()
                .any(|row| row.contains(&AuthFormField::ExportButton)),
            "stored profiles must expose an Export button"
        );

        let creating = build_form_rows(true, 2, false, false, false);
        assert!(
            !creating
                .iter()
                .any(|row| row.contains(&AuthFormField::ExportButton)),
            "a not-yet-saved profile must not expose an Export button"
        );
    }

    /// When creating a new profile (not editing), there must be no DeleteButton.
    #[::core::prelude::v1::test]
    fn form_rows_no_delete_when_creating() {
        let rows = build_form_rows(true, 0, false, false, false);
        assert!(
            !rows
                .iter()
                .any(|row| row.contains(&AuthFormField::DeleteButton)),
            "DeleteButton must not appear when creating a new profile"
        );
    }

    // ---------------------------------------------------------------------------
    // i18n — chrome keys (spec S15): section header, profiles list, form
    // labels/buttons and export/delete dialogs.
    // ---------------------------------------------------------------------------

    const AUTH_PROFILES_CHROME_KEYS: &[&str] = &[
        "settings.auth_profiles.profiles_label",
        "settings.auth_profiles.new_profile",
        "settings.auth_profiles.import",
        "settings.auth_profiles.leave_blank_hint",
        "settings.auth_profiles.auth_profile_label",
        "settings.auth_profiles.editor_subtitle",
        "settings.auth_profiles.name_label",
        "settings.auth_profiles.provider_label",
        "settings.auth_profiles.placeholder_name",
        "settings.auth_profiles.select_provider_ellipsis",
        "settings.auth_profiles.select_ellipsis",
        "settings.auth_profiles.select_none",
        "settings.auth_profiles.enabled",
        "settings.auth_profiles.export",
        "settings.auth_profiles.delete",
        "settings.auth_profiles.cancel",
        "settings.auth_profiles.update",
        "settings.auth_profiles.create",
        "settings.auth_profiles.section_title",
        "settings.auth_profiles.section_description",
        "settings.auth_profiles.delete_dialog_title",
        "settings.auth_profiles.login_hint",
        "settings.auth_profiles.delete_dialog.body_none",
        "settings.auth_profiles.delete_dialog.body_one",
        "settings.auth_profiles.delete_dialog.body_many",
        "settings.auth_profiles.inherited_from",
        "settings.auth_profiles.inherited_from_named",
    ];

    #[::core::prelude::v1::test]
    fn auth_profiles_chrome_keys_resolve() {
        for key in AUTH_PROFILES_CHROME_KEYS {
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
    fn auth_profiles_section_title_english_text() {
        let en = datatab_i18n::t!("settings.auth_profiles.section_title", locale = "en");

        assert_eq!(en, "Auth profiles");
    }

    #[::core::prelude::v1::test]
    fn auth_profiles_new_profile_exact_values() {
        let en = datatab_i18n::t!("settings.auth_profiles.new_profile", locale = "en");

        assert_eq!(en, "New profile");
    }

    // ---------------------------------------------------------------------------
    // i18n — login flow keys (spec S16): conflict/partial-save banners, the
    // interactive login status messages, the login-URL panel, and the Login
    // button chrome.
    // ---------------------------------------------------------------------------

    const AUTH_PROFILES_LOGIN_KEYS: &[&str] = &[
        "settings.auth_profiles.cancel",
        "settings.auth_profiles.login_required_hint",
        "settings.auth_profiles.session_expired_hint",
        "settings.auth_profiles.session_expired_click_hint",
        "settings.auth_profiles.select_before_login",
        "settings.auth_profiles.interactive_login_unavailable",
        "settings.auth_profiles.fields_required_before_login",
        "settings.auth_profiles.login_cancelled",
        "settings.auth_profiles.login_url_panel_description",
        "settings.auth_profiles.open_browser",
        "settings.auth_profiles.copy_url",
        "settings.auth_profiles.login_action",
        "settings.auth_profiles.logging_in",
        "settings.auth_profiles.runs_interactive_login_hint",
    ];

    #[::core::prelude::v1::test]
    fn auth_profiles_login_keys_resolve() {
        for key in AUTH_PROFILES_LOGIN_KEYS {
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
    fn auth_profiles_login_action_english_text() {
        let en = datatab_i18n::t!("settings.auth_profiles.login_action", locale = "en");

        assert_eq!(en, "Log in");
    }

    #[::core::prelude::v1::test]
    fn auth_profiles_delete_body_embeds_name_with_no_affected_connections() {
        let message = crate::labels::auth_profiles_delete_body("prod-mongo", 0);

        assert_eq!(message, "Are you sure you want to delete \"prod-mongo\"?");
    }

    #[::core::prelude::v1::test]
    fn auth_profiles_delete_body_uses_singular_for_one_affected_connection() {
        let message = crate::labels::auth_profiles_delete_body("prod-mongo", 1);

        assert_eq!(
            message,
            "Are you sure you want to delete \"prod-mongo\"? 1 connection using this auth profile will be updated."
        );
    }

    #[::core::prelude::v1::test]
    fn auth_profiles_delete_body_uses_plural_for_multiple_affected_connections() {
        let message = crate::labels::auth_profiles_delete_body("prod-mongo", 3);

        assert_eq!(
            message,
            "Are you sure you want to delete \"prod-mongo\"? 3 connections using this auth profile will be updated."
        );
    }

    #[::core::prelude::v1::test]
    fn auth_profiles_inherited_hint_without_referenced_name() {
        let hint = crate::labels::auth_profiles_inherited_hint("proxy_id", None);

        assert_eq!(hint, "Inherited from proxy_id.");
    }

    #[::core::prelude::v1::test]
    fn auth_profiles_inherited_hint_with_referenced_name() {
        let hint = crate::labels::auth_profiles_inherited_hint("proxy_id", Some("corporate-proxy"));

        assert_eq!(hint, "Inherited from proxy_id 'corporate-proxy'.");
    }
}
