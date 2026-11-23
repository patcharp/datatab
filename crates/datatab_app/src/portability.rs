//! Connection-export/import bridge between `datatab_portability` and `AppState`.
//!
//! This module owns the two seam implementations that the portability crate
//! requires from the app layer — `FieldHintResolver` and `SecretReader` — the
//! function that assembles an `ExportGraph` from AppState data, and the
//! `apply_import` orchestration that persists `ImportActions` through the
//! existing repositories and keyring.
//!
//! # Testability contract
//!
//! All public items in this module depend only on plain data or on trait objects
//! that can be satisfied by fakes in unit tests.  No GPUI `Entity` or `Context`
//! types appear here, which is what allows the `datatab_app` test binary to
//! compile and run these tests (unlike `datatab_ui_windows`, whose GPUI proc-macro
//! expansion causes rustc to SIGSEGV during test-binary compilation).
//!
//! # Import persistence invariants
//!
//! `apply_import` persists `ImportActions` with ordered best-effort writes and
//! NO DB+keyring two-phase commit (2PC).  The insertion order is:
//!
//! 1. Auth profiles (so later connection foreign-key-like references exist)
//! 2. SSH tunnel profiles
//! 3. Proxy profiles
//! 4. Connections (referencing all of the above)
//! 5. Secret writes via `SecretManager::set_by_ref`
//!
//! A `false` return from `set_by_ref` (keyring locked or unavailable) is
//! captured in `ImportOutcome::secret_failures` — it is NEVER silently
//! discarded.  Connections whose `driver_id` is unregistered are recorded in
//! `ImportOutcome::needs_driver` and NOT persisted with a placeholder config.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use datatab_core::secrecy::{ExposeSecret, SecretString};
use datatab_core::{
    AuthProfile, ConnectionProfile, DbDriver, ExportFieldHint, FieldExportTransform, FormValues,
    ProxyProfile, SecretStore, SshTunnelProfile,
};
use datatab_portability::{
    ConnectionWithValues, DestSnapshot, ExportGraph, ExportTransformResolver, FieldHintResolver,
    ImportActions, ImportPlan, ParsedBundle, ResolutionChoices, SecretReader,
};

fn export_driver<'a>(
    profile: &ConnectionProfile,
    drivers: &'a HashMap<String, Arc<dyn DbDriver>>,
) -> Option<&'a Arc<dyn DbDriver>> {
    // A built-in profile without a driver id uses its kind's driver; an
    // external one without an id has no form to look up.
    if matches!(profile.config, datatab_core::DbConfig::External { .. }) {
        profile
            .driver_id
            .as_deref()
            .and_then(|driver_id| drivers.get(driver_id))
    } else {
        drivers.get(&profile.driver_id())
    }
}

// ---------------------------------------------------------------------------
// FieldHintResolver — backed by a driver snapshot
// ---------------------------------------------------------------------------

/// Resolves export field hints by delegating to the registered driver.
///
/// The driver registry is captured as a snapshot (`HashMap`) at the point the
/// user presses "Export", so this type can be sent across threads without
/// holding a reference to `AppState`.  No `driver_id` string branching occurs
/// here; the driver is looked up by its registered ID and the trait method is
/// called generically.
pub struct AppFieldHintResolver {
    drivers: HashMap<String, Arc<dyn DbDriver>>,
}

impl AppFieldHintResolver {
    pub fn new(drivers: HashMap<String, Arc<dyn DbDriver>>) -> Self {
        Self { drivers }
    }
}

impl FieldHintResolver for AppFieldHintResolver {
    fn hint(
        &self,
        profile: &ConnectionProfile,
        field_id: &str,
        values: &FormValues,
    ) -> ExportFieldHint {
        let Some(driver) = export_driver(profile, &self.drivers) else {
            // Without the driver's form nothing says which values are
            // secret, so none of them is written as a value, and a literal
            // value ref is never exported either: every field is a required
            // reference.
            return ExportFieldHint::Secret;
        };

        driver.export_field_hint(field_id, values)
    }
}

// ---------------------------------------------------------------------------
// ExportTransformResolver — backed by the same driver snapshot
// ---------------------------------------------------------------------------

/// Resolves per-field export transforms by delegating to the registered driver.
///
/// Shares the same `drivers` snapshot as `AppFieldHintResolver`. The transform
/// is consulted before the hint in the export loop; drivers that embed credentials
/// in URI fields return `SplitSecret` for those fields.
pub struct AppExportTransformResolver {
    drivers: HashMap<String, Arc<dyn DbDriver>>,
}

impl AppExportTransformResolver {
    pub fn new(drivers: HashMap<String, Arc<dyn DbDriver>>) -> Self {
        Self { drivers }
    }
}

impl ExportTransformResolver for AppExportTransformResolver {
    fn transform(
        &self,
        profile: &ConnectionProfile,
        field_id: &str,
        values: &FormValues,
    ) -> FieldExportTransform {
        let Some(driver) = export_driver(profile, &self.drivers) else {
            return FieldExportTransform::None;
        };

        driver.export_field_transform(field_id, values)
    }
}

// ---------------------------------------------------------------------------
// SecretReader — backed by a shared SecretStore arc
// ---------------------------------------------------------------------------

/// Reads secrets from the OS keyring through a shared `SecretStore`.
///
/// The `Arc<RwLock<...>>` is cloned from `AppState`'s secret store before
/// spawning the background export task, so no borrow of `AppState` is held
/// across await points.
///
/// Returns `None` when the keyring is locked or the entry is absent; never
/// panics.
pub struct AppSecretReader {
    store: Arc<RwLock<Box<dyn SecretStore>>>,
}

impl AppSecretReader {
    pub fn new(store: Arc<RwLock<Box<dyn SecretStore>>>) -> Self {
        Self { store }
    }
}

impl SecretReader for AppSecretReader {
    fn read(&self, secret_ref: &str) -> Option<SecretString> {
        let store = self.store.read().ok()?;
        store.get(secret_ref).ok().flatten()
    }
}

// ---------------------------------------------------------------------------
// ExportGraph assembly
// ---------------------------------------------------------------------------

/// Input data needed to assemble an `ExportGraph`.
///
/// The caller (UI layer) extracts this from `AppState` and passes it here so
/// the assembly function has no dependency on GPUI types. `Default` yields an
/// empty graph, which a standalone-profile export fills with a single entry.
#[derive(Default)]
pub struct ExportInputs {
    /// Selected connection profiles with their driver-extracted form values.
    pub connections_with_values: Vec<(ConnectionProfile, FormValues)>,
    /// Stored auth profiles referenced by any selected connection.
    pub auth_profiles: Vec<AuthProfile>,
    /// SSH tunnel profiles referenced by any selected connection.
    pub ssh_tunnels: Vec<SshTunnelProfile>,
    /// Proxy profiles referenced by any selected connection.
    pub proxies: Vec<ProxyProfile>,
}

/// Assemble an `ExportGraph` from pre-extracted app-state inputs.
///
/// Borrows from `inputs` to produce a graph whose lifetimes are tied to the
/// `inputs` slice data.  The caller owns the data and is responsible for
/// keeping `inputs` alive for the duration of the export call.
pub fn build_export_graph(inputs: &ExportInputs) -> ExportGraph<'_> {
    let connections: Vec<ConnectionWithValues<'_>> = inputs
        .connections_with_values
        .iter()
        .map(|(profile, values)| ConnectionWithValues {
            profile,
            values: values.clone(),
        })
        .collect();

    ExportGraph {
        connections,
        auth_profiles: inputs.auth_profiles.iter().collect(),
        ssh_tunnels: inputs.ssh_tunnels.iter().collect(),
        proxies: inputs.proxies.iter().collect(),
    }
}

// ---------------------------------------------------------------------------
// Import orchestration — DestSnapshot builder + apply_import
// ---------------------------------------------------------------------------

/// Snapshot of the existing destination profiles used for conflict detection.
///
/// The four owned `Vec`s hold clones taken just before `plan()` so the data
/// is stable for the entire import flow.
pub struct OwnedDestSnapshot {
    pub auth_profiles: Vec<AuthProfile>,
    pub ssh_tunnels: Vec<SshTunnelProfile>,
    pub proxies: Vec<ProxyProfile>,
    pub connections: Vec<ConnectionProfile>,
}

impl OwnedDestSnapshot {
    /// Borrow the snapshot as a `DestSnapshot<'_>` suitable for passing to
    /// `datatab_portability::plan()`.
    pub fn as_ref_snapshot(&self) -> DestSnapshot<'_> {
        DestSnapshot {
            auth_profiles: self.auth_profiles.iter().collect(),
            ssh_tunnels: self.ssh_tunnels.iter().collect(),
            proxies: self.proxies.iter().collect(),
            connections: self.connections.iter().collect(),
        }
    }
}

/// The result of a completed import apply.
///
/// Entities listed in `succeeded` were inserted into the repository and their
/// secrets written to the keyring (where available).  Items in
/// `secret_failures` were inserted but at least one secret write was rejected
/// by the OS keyring (returned `false` from `set_by_ref`).  Items in
/// `needs_driver` were skipped entirely because their `driver_id` is not
/// registered in the current driver registry.  Items in `config_failures` were
/// skipped because the registered driver's `build_config` call returned an error —
/// the UI surfaces both buckets as informational notes.
#[derive(Debug, Default)]
pub struct ImportOutcome {
    /// Names of entities that were fully persisted.
    pub succeeded: Vec<String>,
    /// `(entity_name, secret_ref)` pairs where the keyring write returned `false`.
    pub secret_failures: Vec<(String, String)>,
    /// `(connection_name, driver_id)` pairs whose driver is not registered.
    pub needs_driver: Vec<(String, String)>,
    /// `(connection_name, error_message)` pairs where `build_config` returned an error.
    pub config_failures: Vec<(String, String)>,
    /// Connection names whose intra-bundle references (ssh/proxy/auth local_ids) could
    /// not be resolved. These connections were NOT imported to prevent silent topology
    /// degradation (e.g. bastion-routed → direct-connect).
    pub unresolved_refs: Vec<String>,
}

/// Result returned by `ImportPersistence::add_connection`.
#[derive(Debug)]
pub enum ConnectionInsertResult {
    /// Connection was inserted successfully.
    Ok,
    /// The `driver_id` is not registered; connection skipped.
    NeedsDriver,
    /// The driver is registered but `build_config` failed; connection skipped.
    ConfigFailed(String),
}

/// Seam for inserting entities into the repository, used by `apply_import`.
///
/// Production uses an owned `ImportSession` for background secret work and
/// foreground state commits; tests can supply other implementations.
pub trait ImportPersistence {
    fn add_auth_profile(&mut self, profile: AuthProfile);
    fn add_ssh_tunnel(&mut self, tunnel: SshTunnelProfile);
    fn add_proxy(&mut self, proxy: ProxyProfile);

    /// Insert a connection profile.
    ///
    /// Returns `ConnectionInsertResult::Ok` on success.
    /// Returns `ConnectionInsertResult::NeedsDriver` when the `driver_id` is not
    /// registered, causing the connection to be recorded in `needs_driver` and skipped.
    /// Returns `ConnectionInsertResult::ConfigFailed(reason)` when the driver is present
    /// but `build_config` fails; the connection is recorded in `config_failures` and
    /// NOT persisted with a broken placeholder config.
    fn add_connection(&mut self, profile: ConnectionProfile) -> ConnectionInsertResult;

    /// Insert a connection with the secret for its password slot
    /// (`connection_secret_ref`), when the bundle carries one.
    ///
    /// An implementation that writes the password itself, in one step with
    /// the profile's other keyring entries, returns `None` for it: a write
    /// that fails then fails the connection, and nothing of it is saved. The
    /// default inserts the connection and hands the password back, and
    /// `persist_import_actions` writes it with the other secrets.
    fn add_connection_with_password(
        &mut self,
        profile: ConnectionProfile,
        password: Option<SecretString>,
    ) -> (ConnectionInsertResult, Option<SecretString>) {
        (self.add_connection(profile), password)
    }

    /// Write a secret under an explicit keyring reference.
    ///
    /// Returns `true` on success, `false` when the keyring is unavailable or
    /// rejected the write.
    fn write_secret(&self, secret_ref: &str, secret: &SecretString) -> bool;

    /// The secret fields this connection keeps in their own keyring entries
    /// instead of the connection password slot: the secret-kind fields other
    /// than `password` of a registered driver that builds an external config.
    ///
    /// A supplied secret for one of these fields is handed to
    /// `add_connection` in the profile's values, which stores it in that
    /// field's entry or refuses the connection. The default keeps every
    /// supplied secret in the password slot.
    fn keyring_secret_field_ids(&self, _profile: &ConnectionProfile) -> Vec<String> {
        Vec::new()
    }

    /// Every secret-kind field of this connection's driver form, the primary
    /// `password` included; empty when the driver is not registered.
    ///
    /// A `Literal` value ref the bundle carries for one of these fields is the
    /// secret itself. The import takes it out of the profile and stores it like
    /// a supplied secret (see `take_literal_secret_refs`), so it never reaches
    /// the database.
    fn secret_field_ids(&self, _profile: &ConnectionProfile) -> Vec<String> {
        Vec::new()
    }

    /// Populate the in-memory `secret_fields` for an auth profile after its secrets
    /// have been written to the keyring.
    ///
    /// Called immediately after `add_auth_profile` for newly created (non-Reuse/MapTo)
    /// profiles so they authenticate in the current session without a restart (H3 / ADR-4).
    /// Production updates the session snapshot and queues hydration of the live
    /// in-memory profile for the foreground `ImportSession::commit`.
    fn hydrate_auth_secret_fields(
        &mut self,
        auth_id: uuid::Uuid,
        fields: HashMap<String, SecretString>,
    );

    /// The driver form values that choosing this auth profile sets, or `None`
    /// when the destination no longer holds the profile.
    ///
    /// The connection ID lets the session re-check this choice at commit.
    /// Asked for a profile the user chose in the import panel, which may have
    /// been deleted since the panel listed it. The values are the ones the
    /// connection form writes on the same choice
    /// (`datatab_core::auth_profile_form_values`); they are empty when the
    /// driver is unknown or declares no `AuthProfileRef` field.
    fn chosen_auth_profile_values(
        &mut self,
        connection_id: uuid::Uuid,
        driver_id: &str,
        auth_id: uuid::Uuid,
    ) -> Option<FormValues>;
}

/// Owned import work. Run the persistence functions with this session on a
/// background executor, then commit its accepted rows on the foreground.
/// Driver and auth snapshots carry no UI entities; the shared keyring is the
/// only external state touched before commit.
pub struct ImportSession {
    drivers: HashMap<String, Arc<dyn DbDriver>>,
    auth_profiles: Vec<AuthProfile>,
    secrets: datatab_core::SecretManager,
    writes: Vec<ImportStateWrite>,
    chosen_auth_profiles: HashMap<uuid::Uuid, uuid::Uuid>,
    field_changes: HashMap<uuid::Uuid, crate::app_state::SecretFieldChanges>,
    rejected_connections: Vec<(
        Box<ConnectionProfile>,
        uuid::Uuid,
        crate::app_state::SecretFieldChanges,
    )>,
    password_keyring_failed: String,
}

enum ImportStateWrite {
    Auth(Box<AuthProfile>),
    Ssh(Box<SshTunnelProfile>),
    Proxy(Box<ProxyProfile>),
    Connection(Box<ConnectionProfile>),
    Hydrate(uuid::Uuid, HashMap<String, SecretString>),
}

fn missing_chosen_auth_profile(auth_id: uuid::Uuid) -> String {
    format!("the chosen auth profile {auth_id} no longer exists")
}

impl ImportSession {
    pub fn new(state: &crate::AppState, password_keyring_failed: String) -> Self {
        Self {
            drivers: state.drivers().clone(),
            auth_profiles: state.list_auth_profiles(),
            secrets: state.facade.secrets.clone(),
            writes: Vec::new(),
            chosen_auth_profiles: HashMap::new(),
            field_changes: HashMap::new(),
            rejected_connections: Vec::new(),
            password_keyring_failed,
        }
    }

    /// Commit only rows accepted by the secret phase, in dependency order.
    pub fn commit(&mut self, state: &mut crate::AppState) {
        for write in self.writes.drain(..) {
            match write {
                ImportStateWrite::Auth(profile) => {
                    state.add_auth_profile(*profile);
                }
                ImportStateWrite::Ssh(profile) => state.add_ssh_tunnel(*profile),
                ImportStateWrite::Proxy(profile) => state.add_proxy(*profile),
                ImportStateWrite::Connection(profile) => {
                    let changes = self.field_changes.remove(&profile.id).unwrap_or_default();
                    if let Some(auth_id) = self.chosen_auth_profiles.remove(&profile.id)
                        && !state
                            .facade
                            .auth_profiles
                            .items
                            .iter()
                            .any(|auth| auth.id == auth_id)
                    {
                        self.rejected_connections.push((profile, auth_id, changes));
                        continue;
                    }
                    state.add_profile_in_folder(*profile, None);
                }
                ImportStateWrite::Hydrate(id, fields) => {
                    if let Some(profile) = state
                        .facade
                        .auth_profiles
                        .items
                        .iter_mut()
                        .find(|profile| profile.id == id)
                    {
                        profile.secret_fields = fields;
                    }
                }
            }
        }
    }

    /// Restore secrets of rows refused at commit. Run on the background executor
    /// and replace these rows' successes with the returned config failures.
    pub fn rollback_rejected(&mut self, cleanup_failed: &str) -> Vec<(uuid::Uuid, String, String)> {
        self.rejected_connections
            .drain(..)
            .map(|(profile, auth_id, changes)| {
                let mut reason = missing_chosen_auth_profile(auth_id);
                let restore_result = changes.restore(&self.secrets.secret_store_arc());
                if self
                    .secrets
                    .try_delete_by_ref(&datatab_core::connection_secret_ref(&profile.id))
                    .is_err()
                {
                    reason = cleanup_failed.to_string();
                }
                if let Err(may_have_changed) = restore_result {
                    reason.push(' ');
                    reason.push_str(&may_have_changed.to_string());
                }
                (profile.id, profile.name, reason)
            })
            .collect()
    }
}

impl ImportPersistence for ImportSession {
    fn add_auth_profile(&mut self, profile: AuthProfile) {
        self.auth_profiles.push(profile.clone());
        self.writes.push(ImportStateWrite::Auth(Box::new(profile)));
    }

    fn add_ssh_tunnel(&mut self, tunnel: SshTunnelProfile) {
        self.writes.push(ImportStateWrite::Ssh(Box::new(tunnel)));
    }

    fn add_proxy(&mut self, proxy: ProxyProfile) {
        self.writes.push(ImportStateWrite::Proxy(Box::new(proxy)));
    }

    fn add_connection(&mut self, profile: ConnectionProfile) -> ConnectionInsertResult {
        let (result, _password) = self.add_connection_with_password(profile, None);
        result
    }

    fn add_connection_with_password(
        &mut self,
        profile: ConnectionProfile,
        password: Option<SecretString>,
    ) -> (ConnectionInsertResult, Option<SecretString>) {
        let driver_id = profile.driver_id().to_string();
        let Some(driver) = self.drivers.get(&driver_id).cloned() else {
            return (ConnectionInsertResult::NeedsDriver, password);
        };

        let datatab_core::DbConfig::External { values, .. } = &profile.config else {
            self.writes
                .push(ImportStateWrite::Connection(Box::new(profile)));
            return (ConnectionInsertResult::Ok, password);
        };
        match driver.build_config(values) {
            Ok(config) => {
                let mut rebuilt = profile;
                rebuilt.config = config;
                self.add_built_connection(rebuilt, password)
            }
            Err(e) => (
                ConnectionInsertResult::ConfigFailed(e.to_string()),
                password,
            ),
        }
    }

    fn write_secret(&self, secret_ref: &str, secret: &SecretString) -> bool {
        self.secrets.set_by_ref(secret_ref, secret)
    }

    fn keyring_secret_field_ids(&self, profile: &ConnectionProfile) -> Vec<String> {
        crate::AppState::imported_keyring_field_ids_with(profile, &self.drivers)
    }

    fn secret_field_ids(&self, profile: &ConnectionProfile) -> Vec<String> {
        crate::AppState::secret_field_ids_with(profile, &self.drivers)
    }

    fn chosen_auth_profile_values(
        &mut self,
        connection_id: uuid::Uuid,
        driver_id: &str,
        auth_id: uuid::Uuid,
    ) -> Option<datatab_core::FormValues> {
        let profile = self
            .auth_profiles
            .iter()
            .find(|profile| profile.id == auth_id)?;
        let values = self
            .drivers
            .get(driver_id)
            .map(|driver| datatab_core::auth_profile_form_values(driver.form_definition(), profile))
            .unwrap_or_default();
        self.chosen_auth_profiles.insert(connection_id, auth_id);
        Some(values)
    }

    fn hydrate_auth_secret_fields(
        &mut self,
        auth_id: uuid::Uuid,
        fields: HashMap<String, SecretString>,
    ) {
        if let Some(profile) = self.auth_profiles.iter_mut().find(|p| p.id == auth_id) {
            profile.secret_fields = fields.clone();
            self.writes.push(ImportStateWrite::Hydrate(auth_id, fields));
        }
    }
}

impl ImportSession {
    /// Saves a profile whose config the driver built. An external profile
    /// whose secret fields this import wrote to the keyring is saved in one
    /// step with its password slot: when the password cannot be written, the
    /// field entries are put back and the profile is not saved. Any other
    /// profile hands its password back, so it is written with the other
    /// secrets after every connection, as before.
    fn add_built_connection(
        &mut self,
        mut profile: ConnectionProfile,
        password: Option<SecretString>,
    ) -> (ConnectionInsertResult, Option<SecretString>) {
        // The registered driver may declare a reserved kind; such a profile
        // could not be loaded again.
        if let Err(error) = crate::AppState::check_profile_can_be_saved(&profile) {
            return (
                ConnectionInsertResult::ConfigFailed(error.to_string()),
                password,
            );
        }
        // A secret field value in the bundle goes to the keyring like a typed
        // one; the new profile has no entries yet.
        profile.keyring_secret_fields.clear();
        // A refused store leaves no entry behind, or names the fields it may
        // have changed.
        let changes = match crate::AppState::prepare_external_secret_fields_with(
            &mut profile,
            &self.drivers,
            self.secrets.secret_store_arc(),
        )
        .apply()
        {
            Ok(changes) => changes,
            Err(error) => {
                return (
                    ConnectionInsertResult::ConfigFailed(error.to_string()),
                    password,
                );
            }
        };
        // Only a profile whose secret fields this import wrote has entries
        // to roll back; any other keeps the old path, where a refused
        // password is a secret failure of a saved profile.
        if !matches!(profile.config, datatab_core::DbConfig::External { .. }) || changes.is_empty()
        {
            self.field_changes.insert(profile.id, changes);
            self.writes
                .push(ImportStateWrite::Connection(Box::new(profile)));
            return (ConnectionInsertResult::Ok, password);
        }
        if let Some(password) = password {
            let password_ref = datatab_core::connection_secret_ref(&profile.id);
            if !self.secrets.set_by_ref(&password_ref, &password) {
                let mut reason = self.password_keyring_failed.clone();
                if let Err(may_have_changed) = changes.restore(&self.secrets.secret_store_arc()) {
                    reason.push(' ');
                    reason.push_str(&may_have_changed.to_string());
                }
                return (ConnectionInsertResult::ConfigFailed(reason), None);
            }
        }
        self.field_changes.insert(profile.id, changes);
        self.writes
            .push(ImportStateWrite::Connection(Box::new(profile)));
        (ConnectionInsertResult::Ok, None)
    }
}

/// Run the pure `datatab_portability::apply()` and persist the resulting
/// `ImportActions` through `deps`.
///
/// This is the final step of the import pipeline.  It is SEPARATE from the
/// portability crate's `apply()` because persistence requires access to the
/// driver registry (to rebuild the real `DbConfig`) and the OS keyring (for
/// `SecretManager::set_by_ref`), neither of which the pure-logic crate owns.
///
/// Persistence order: auth profiles → SSH tunnels → proxy profiles →
/// connections → secrets.  This order ensures FK-like references exist before
/// they are pointed to.
///
/// Any connection whose `driver_id` is not registered by `deps.add_connection`
/// is skipped and recorded in `ImportOutcome::needs_driver`; the import
/// continues for all remaining entities.
pub fn apply_import(
    parsed: &ParsedBundle,
    plan: &ImportPlan,
    choices: &ResolutionChoices,
    deps: &mut dyn ImportPersistence,
) -> Result<ImportOutcome, datatab_portability::PortabilityError> {
    let actions = datatab_portability::import::apply(parsed, plan, choices)?;
    let outcome = persist_import_actions(actions, deps);
    Ok(outcome)
}

/// Persist `ImportActions` through the `ImportPersistence` seam.
///
/// Separated from `apply_import` so tests can call it directly with a
/// pre-built `ImportActions` without needing a full `ParsedBundle`.
///
/// # Hydration (H3 / ADR-4)
///
/// After writing auth-profile secrets to the keyring, this function populates the
/// in-memory `secret_fields` on newly created auth profiles via `hydrate_auth_secret_fields`.
/// This allows the imported profile to authenticate in the current session without restart.
/// Reuse/MapTo profiles are excluded — they retain their destination credentials (ADR-6 / H2).
///
/// # Secret-write filtering
///
/// Secrets are written only for connections that were successfully persisted.
/// Connections recorded in `needs_driver` or `config_failures` are skipped, and
/// their `connection_secret_ref`-keyed entries are filtered out of `secret_writes`
/// to prevent orphaned keyring entries for profiles that were never inserted.
pub fn persist_import_actions(
    actions: ImportActions,
    deps: &mut dyn ImportPersistence,
) -> ImportOutcome {
    let mut outcome = ImportOutcome::default();

    // Record kind, dangling-ref and value-ref failures reported by the pure apply() step.
    for (name, driver_id) in actions.kind_failures {
        outcome.config_failures.push((
            name,
            format!("missing or unparseable `kind` field (driver: {driver_id})"),
        ));
    }
    for name in actions.unresolved_ref_connections {
        outcome.unresolved_refs.push(name);
    }
    for (name, fields) in actions.value_ref_failures {
        outcome.config_failures.push((
            name,
            format!("unreadable value reference for: {}", fields.join(", ")),
        ));
    }

    // Collect (new_auth_id, field_name) -> secret pairs for in-session hydration.
    // We capture these before writing so we can populate secret_fields post-write.
    let mut auth_secret_by_id: HashMap<uuid::Uuid, HashMap<String, SecretString>> = HashMap::new();

    // Stage: identify which auth secret_writes belong to the new auth profiles.
    // Auth secret refs use the format `datatab:auth:<uuid>:<field>`.
    for (ref_str, secret) in &actions.secret_writes {
        if let Some(stripped) = ref_str.strip_prefix("datatab:auth:")
            && let Some((uuid_str, field)) = stripped.split_once(':')
            && let Ok(auth_id) = uuid::Uuid::parse_str(uuid_str)
        {
            auth_secret_by_id
                .entry(auth_id)
                .or_default()
                .insert(field.to_string(), secret.clone());
        }
    }

    for auth in actions.auth_profiles {
        let name = auth.name.clone();
        let auth_id = auth.id;
        deps.add_auth_profile(auth);
        outcome.succeeded.push(name);

        // H3 / ADR-4: hydrate in-memory secret_fields for the newly added profile.
        if let Some(fields) = auth_secret_by_id.get(&auth_id) {
            deps.hydrate_auth_secret_fields(auth_id, fields.clone());
        }
    }

    for ssh in actions.ssh_tunnels {
        let name = ssh.name.clone();
        deps.add_ssh_tunnel(ssh);
        outcome.succeeded.push(name);
    }

    for proxy in actions.proxies {
        let name = proxy.name.clone();
        deps.add_proxy(proxy);
        outcome.succeeded.push(name);
    }

    // Track UUIDs of skipped connections to filter their secret writes below.
    let mut skipped_conn_ids: std::collections::HashSet<uuid::Uuid> =
        std::collections::HashSet::new();

    let mut field_secrets_by_connection: HashMap<uuid::Uuid, Vec<(String, SecretString)>> =
        HashMap::new();
    for (connection_id, field_id, secret) in actions.connection_field_secrets {
        field_secrets_by_connection
            .entry(connection_id)
            .or_default()
            .push((field_id, secret));
    }
    let mut password_slot_writes: Vec<(String, SecretString)> = Vec::new();
    let mut secret_writes = actions.secret_writes;

    for mut conn in actions.connections {
        let name = conn.name.clone();
        let driver_id = conn.driver_id().to_string();
        let conn_id = conn.id;

        take_literal_secret_refs(&mut conn, deps, &mut password_slot_writes);
        route_supplied_field_secrets(
            &mut conn,
            field_secrets_by_connection
                .remove(&conn_id)
                .unwrap_or_default(),
            deps,
            &mut password_slot_writes,
        );

        if let Some((_, auth_id)) = actions
            .chosen_auth_profiles
            .iter()
            .find(|(connection_id, _)| *connection_id == conn_id)
        {
            match deps.chosen_auth_profile_values(conn_id, &driver_id, *auth_id) {
                Some(values) => {
                    if let datatab_core::DbConfig::External {
                        values: form_values,
                        ..
                    } = &mut conn.config
                    {
                        form_values.extend(values);
                    }
                }
                None => {
                    outcome
                        .config_failures
                        .push((name, missing_chosen_auth_profile(*auth_id)));
                    skipped_conn_ids.insert(conn_id);
                    continue;
                }
            }
        }

        let password_ref = datatab_core::connection_secret_ref(&conn_id);
        let password =
            take_password_slot_write(&password_ref, &mut secret_writes, &mut password_slot_writes);
        let (result, password) = deps.add_connection_with_password(conn, password);
        if let Some(password) = password {
            password_slot_writes.push((password_ref, password));
        }

        match result {
            ConnectionInsertResult::Ok => {
                outcome.succeeded.push(name);
            }
            ConnectionInsertResult::NeedsDriver => {
                outcome.needs_driver.push((name, driver_id));
                skipped_conn_ids.insert(conn_id);
            }
            ConnectionInsertResult::ConfigFailed(reason) => {
                outcome.config_failures.push((name, reason));
                skipped_conn_ids.insert(conn_id);
            }
        }
    }

    // Precompute the exact `connection_secret_ref` string for each skipped id so the
    // filter below uses an exact prefix match rather than a loose substring search.
    // `connection_secret_ref` produces the canonical `"datatab:conn:<uuid>"` prefix that
    // all connection-scoped secret refs begin with.
    let skipped_conn_refs: std::collections::HashSet<String> = skipped_conn_ids
        .iter()
        .map(datatab_core::connection_secret_ref)
        .collect();

    for (secret_ref, secret) in secret_writes.into_iter().chain(password_slot_writes) {
        // Skip secrets belonging to connections that were not persisted.
        let is_skipped = skipped_conn_refs
            .iter()
            .any(|prefix| secret_ref.starts_with(prefix.as_str()));

        if is_skipped {
            continue;
        }

        if !deps.write_secret(&secret_ref, &secret) {
            outcome
                .secret_failures
                .push(("(secret)".to_string(), secret_ref));
        }
    }

    outcome
}

/// Takes every write to one connection's password slot out of the pending
/// writes and returns the one that would have landed last, so the
/// connection can be saved in one step with it.
fn take_password_slot_write(
    password_ref: &str,
    secret_writes: &mut Vec<(String, SecretString)>,
    password_slot_writes: &mut Vec<(String, SecretString)>,
) -> Option<SecretString> {
    let mut password = None;
    for writes in [secret_writes, password_slot_writes] {
        let (matching, rest): (Vec<_>, Vec<_>) = std::mem::take(writes)
            .into_iter()
            .partition(|(secret_ref, _)| secret_ref == password_ref);
        *writes = rest;
        if let Some((_, secret)) = matching.into_iter().last() {
            password = Some(secret);
        }
    }
    password
}

/// Takes the `Literal` value refs on secret-kind fields out of an imported
/// profile and stores each value like a typed one: the password goes to the
/// connection password slot, and a field the driver keeps in its own keyring
/// entry goes into the profile's values, where `add_connection` writes it to
/// that entry. A literal on any other secret field has no slot and is
/// dropped. The count is logged, never a value. A literal on a field that is
/// not secret stays a value ref.
fn take_literal_secret_refs(
    profile: &mut ConnectionProfile,
    deps: &dyn ImportPersistence,
    password_slot_writes: &mut Vec<(String, SecretString)>,
) {
    let secret_fields = deps.secret_field_ids(profile);
    let keyring_fields = deps.keyring_secret_field_ids(profile);
    let literal_fields: Vec<String> = profile
        .value_refs
        .iter()
        .filter(|(field_id, value_ref)| {
            matches!(value_ref, datatab_core::ValueRef::Literal { .. })
                && secret_fields.contains(field_id)
        })
        .map(|(field_id, _)| field_id.clone())
        .collect();
    let mut dropped = 0;
    for field_id in literal_fields {
        let Some(datatab_core::ValueRef::Literal { value }) = profile.value_refs.remove(&field_id)
        else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        if field_id == "password" {
            if profile.save_password {
                password_slot_writes.push((
                    datatab_core::connection_secret_ref(&profile.id),
                    SecretString::from(value),
                ));
            } else {
                dropped += 1;
            }
        } else if keyring_fields.contains(&field_id)
            && let datatab_core::DbConfig::External { values, .. } = &mut profile.config
        {
            values.insert(field_id, value);
        } else {
            // Defensive: the bundle carries a literal only for a field with
            // a slot, and the exporter never writes one. A hand-edited
            // bundle could, so the value has nowhere to go and is dropped.
            dropped += 1;
        }
    }
    if dropped > 0 {
        log::warn!("Dropped {dropped} literal secret value(s) of an imported connection");
    }
}

/// Routes the secrets the user supplied for one connection. A field the
/// driver keeps in its own keyring entry goes into the profile's values, so
/// `add_connection` stores it there (or refuses the connection) before the
/// profile is written; it never reaches the password slot. Any other field
/// goes to the connection password slot, as before.
fn route_supplied_field_secrets(
    profile: &mut ConnectionProfile,
    field_secrets: Vec<(String, SecretString)>,
    deps: &dyn ImportPersistence,
    password_slot_writes: &mut Vec<(String, SecretString)>,
) {
    if field_secrets.is_empty() {
        return;
    }
    let keyring_fields = deps.keyring_secret_field_ids(profile);
    for (field_id, secret) in field_secrets {
        if keyring_fields.contains(&field_id)
            && let datatab_core::DbConfig::External { values, .. } = &mut profile.config
        {
            values.insert(field_id, secret.expose_secret().to_string());
        } else {
            password_slot_writes.push((datatab_core::connection_secret_ref(&profile.id), secret));
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 4.1 — M7: MapTo candidate computation
// ---------------------------------------------------------------------------

/// Return all destination profiles of the same `ConflictKind` as the given conflict,
/// suitable for the "Map to another" picker in the import wizard.
///
/// MapTo (choose a different destination profile) is distinct from Reuse (keep the
/// conflicting one). This function returns ALL same-kind profiles at the destination,
/// not only the conflicting one. An empty list means only Reuse or CreateNew are
/// available (ADR-3 / M7 / R-WIZ-3).
pub fn mapto_candidates(
    kind: datatab_portability::ConflictKind,
    dest: &OwnedDestSnapshot,
) -> Vec<(uuid::Uuid, String)> {
    match kind {
        datatab_portability::ConflictKind::AuthProfile => dest
            .auth_profiles
            .iter()
            .map(|a| (a.id, a.name.clone()))
            .collect(),
        datatab_portability::ConflictKind::SshTunnel => dest
            .ssh_tunnels
            .iter()
            .map(|s| (s.id, s.name.clone()))
            .collect(),
        datatab_portability::ConflictKind::Proxy => dest
            .proxies
            .iter()
            .map(|p| (p.id, p.name.clone()))
            .collect(),
        datatab_portability::ConflictKind::Connection => dest
            .connections
            .iter()
            .map(|c| (c.id, c.name.clone()))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Phase 4.2 — C3: ConfirmSummary — computed once at plan time for stable rendering
// ---------------------------------------------------------------------------

/// Summary of entities to be imported, computed at plan time.
///
/// The wizard renders from this value rather than from the live `parsed_bundle`,
/// so the Confirm step shows correct counts even after `parsed_bundle` is consumed
/// by `do_apply` (ADR-3 / C3 / R-WIZ-1).
#[derive(Debug, Clone, Default)]
pub struct ConfirmSummary {
    /// Number of connections to be imported (including new + reuse + conflict-resolved).
    pub connection_count: usize,
    /// Number of auth profiles to be imported or reused.
    pub auth_profile_count: usize,
    /// Number of SSH tunnel profiles to be imported or reused.
    pub ssh_tunnel_count: usize,
    /// Number of proxy profiles to be imported or reused.
    pub proxy_count: usize,
    /// Number of required resolutions the user must supply before applying.
    pub required_resolution_count: usize,
    /// Number of connection conflicts detected (Reuse/CreateNew/MapTo choices pending).
    pub conflict_count: usize,
    /// True when any connection in the bundle targets a driver not in the registry.
    /// The wizard can surface a note about installing the relevant driver.
    pub has_driver_not_installed: bool,
}

/// Compute a `ConfirmSummary` from the plan and bundle.
///
/// Called immediately after `plan()` returns so the wizard can render the Confirm
/// step from a stable snapshot rather than the live `ParsedBundle`.
pub fn confirm_summary(
    parsed: &datatab_portability::ParsedBundle,
    plan: &datatab_portability::ImportPlan,
    registered_drivers: &std::collections::HashSet<String>,
) -> ConfirmSummary {
    let has_driver_not_installed = parsed
        .bundle
        .connections
        .iter()
        .any(|c| !registered_drivers.contains(&c.driver_id));

    ConfirmSummary {
        connection_count: parsed.bundle.connections.len(),
        auth_profile_count: parsed.bundle.auth_profiles.len(),
        ssh_tunnel_count: parsed.bundle.ssh_tunnels.len(),
        proxy_count: parsed.bundle.proxies.len(),
        required_resolution_count: plan.required_resolutions.len(),
        conflict_count: plan.conflicts.len(),
        has_driver_not_installed,
    }
}

// ---------------------------------------------------------------------------
// External client import — pure candidate mapping and per-candidate
// persistence, reusing the same `ImportPersistence` seam as bundle import.
// ---------------------------------------------------------------------------

/// Result of persisting a batch of confirmed external-import candidates.
#[derive(Debug, Clone, Default)]
pub struct ExternalPersistOutcome {
    /// Names of candidates that were fully persisted.
    pub succeeded: Vec<String>,
    /// Names of persisted candidates whose secret failed to write to the keyring.
    pub secret_failures: Vec<String>,
    /// `(candidate_name, driver_id)` pairs whose driver is not registered.
    pub needs_driver: Vec<(String, String)>,
    /// `(candidate_name, error_message)` pairs where `build_config` returned an error.
    pub config_failures: Vec<(String, String)>,
}

/// Maps one parsed external candidate into a `ConnectionProfile`.
///
/// Pure mapping: `save_password` is derived from whether a secret was
/// recovered from the source format. The profile id is freshly generated —
/// external client formats have no id datatab can reuse across imports.
pub fn external_candidate_to_profile(
    candidate: &datatab_portability::external::ExternalImportCandidate,
) -> ConnectionProfile {
    let mut profile = ConnectionProfile::new_with_kind(
        candidate.name.clone(),
        candidate.kind,
        candidate.config.clone(),
    );
    profile.save_password = candidate.secret.is_some();
    profile
}

/// Persists one confirmed external candidate through `deps`, writing its
/// recovered secret (when present) under the new connection's own
/// `connection_secret_ref`.
///
/// Takes the candidate by value because its `secret` field is not `Clone`
/// (`secrecy::SecretString` deliberately does not implement `Clone` for
/// string payloads); this mirrors the `Option::take()` ownership pattern
/// used elsewhere in the import pipeline.
pub fn persist_external_candidate(
    candidate: datatab_portability::external::ExternalImportCandidate,
    deps: &mut dyn ImportPersistence,
    outcome: &mut ExternalPersistOutcome,
) {
    let profile = external_candidate_to_profile(&candidate);
    let name = candidate.name;
    let secret = candidate.secret;
    let profile_id = profile.id;
    let driver_id = profile.driver_id().to_string();

    match deps.add_connection(profile) {
        ConnectionInsertResult::Ok => {
            outcome.succeeded.push(name.clone());

            if let Some(secret) = secret {
                let secret_ref = datatab_core::connection_secret_ref(&profile_id);
                if !deps.write_secret(&secret_ref, &secret) {
                    outcome.secret_failures.push(name);
                }
            }
        }
        ConnectionInsertResult::NeedsDriver => {
            outcome.needs_driver.push((name, driver_id));
        }
        ConnectionInsertResult::ConfigFailed(reason) => {
            outcome.config_failures.push((name, reason));
        }
    }
}

/// Selection-state reducer for the external-import review checklist: one
/// include flag per parsed candidate, defaulting to included.
pub fn default_candidate_includes(count: usize) -> Vec<bool> {
    vec![true; count]
}

/// Toggles the include flag for one candidate index.
///
/// A no-op on an out-of-range index rather than a panic, since the index
/// always comes from a rendered row that matches the current candidate list.
pub fn toggle_candidate_include(includes: &mut [bool], index: usize) {
    if let Some(flag) = includes.get_mut(index) {
        *flag = !*flag;
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    use datatab_core::secrecy::SecretString;
    use datatab_core::{
        AuthProfile, Connection, ConnectionProfile, DbConfig, DbError, DbKind, DriverFormDef,
        DriverMetadata, ExportFieldHint, FormValues, ProxyProfile, SecretStore, SshTunnelProfile,
    };
    use datatab_portability::{FieldHintResolver, SecretReader};
    use uuid::Uuid;

    // -----------------------------------------------------------------------
    // Fake driver — implements only the methods needed for export tests.
    // Required methods that are irrelevant here are marked `unimplemented!()`.
    // -----------------------------------------------------------------------

    fn empty_form_def() -> DriverFormDef {
        DriverFormDef { tabs: vec![] }
    }

    struct FakeDriver {
        /// When `Some`, returned for any field whose ID starts with `"secret_"`.
        secret_field_hint: Option<ExportFieldHint>,
        form_def: DriverFormDef,
    }

    impl FakeDriver {
        fn new_with_hint(hint: ExportFieldHint) -> Self {
            Self {
                secret_field_hint: Some(hint),
                form_def: empty_form_def(),
            }
        }

        fn new_default() -> Self {
            Self {
                secret_field_hint: None,
                form_def: empty_form_def(),
            }
        }
    }

    // Only the form/field methods below are exercised by the export tests; the
    // remaining trait methods are deliberately unimplemented on this stub.
    #[expect(
        clippy::unimplemented,
        reason = "export tests stub: they exercise only `form_definition`, \
                  `extract_values` and `export_field_hint` and never call the \
                  methods marked `unimplemented!` below"
    )]
    impl datatab_core::DbDriver for FakeDriver {
        fn kind(&self) -> DbKind {
            DbKind::SQLite
        }

        fn metadata(&self) -> &DriverMetadata {
            unimplemented!("FakeDriver::metadata not needed in export tests")
        }

        fn form_definition(&self) -> &DriverFormDef {
            &self.form_def
        }

        fn driver_key(&self) -> String {
            "fake".to_string()
        }

        fn build_config(&self, _values: &FormValues) -> Result<DbConfig, DbError> {
            unimplemented!("FakeDriver::build_config not needed in export tests")
        }

        fn extract_values(&self, _config: &DbConfig) -> FormValues {
            FormValues::default()
        }

        fn export_field_hint(&self, field_id: &str, _values: &FormValues) -> ExportFieldHint {
            match &self.secret_field_hint {
                Some(hint) if field_id.starts_with("secret_") => *hint,
                _ => ExportFieldHint::Include,
            }
        }

        fn connect_with_secrets(
            &self,
            _profile: &ConnectionProfile,
            _password: Option<&SecretString>,
            _ssh_secret: Option<&SecretString>,
        ) -> Result<Box<dyn Connection>, DbError> {
            unimplemented!("FakeDriver::connect_with_secrets not needed in export tests")
        }

        fn test_connection(&self, _profile: &ConnectionProfile) -> Result<(), DbError> {
            unimplemented!("FakeDriver::test_connection not needed in export tests")
        }
    }

    // -----------------------------------------------------------------------
    // Fake SecretStore
    // -----------------------------------------------------------------------

    struct FakeSecretStore {
        secrets: HashMap<String, String>,
    }

    impl FakeSecretStore {
        fn with(pairs: &[(&str, &str)]) -> Self {
            Self {
                secrets: pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }

        fn empty() -> Self {
            Self {
                secrets: HashMap::new(),
            }
        }
    }

    impl SecretStore for FakeSecretStore {
        fn is_available(&self) -> bool {
            true
        }

        fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, DbError> {
            Ok(self
                .secrets
                .get(secret_ref)
                .map(|v| SecretString::from(v.clone())))
        }

        fn set(&self, _secret_ref: &str, _value: &SecretString) -> Result<(), DbError> {
            Ok(())
        }

        fn delete(&self, _secret_ref: &str) -> Result<(), DbError> {
            Ok(())
        }
    }

    fn make_store_arc(store: FakeSecretStore) -> Arc<RwLock<Box<dyn SecretStore>>> {
        Arc::new(RwLock::new(Box::new(store)))
    }

    // -----------------------------------------------------------------------
    // Profile constructors (no Default impl on these types)
    // -----------------------------------------------------------------------

    fn make_profile(driver_id: &str) -> ConnectionProfile {
        let mut p = ConnectionProfile::new(
            "test",
            DbConfig::External {
                kind: DbKind::SQLite,
                values: FormValues::default(),
            },
        );
        p.driver_id = Some(driver_id.to_string());
        p
    }

    fn make_auth_profile(id: Uuid) -> AuthProfile {
        AuthProfile {
            id,
            name: "test-auth".to_string(),
            provider_id: "test-provider".to_string(),
            fields: HashMap::new(),
            secret_fields: HashMap::new(),
            enabled: true,
        }
    }

    fn make_ssh_tunnel(id: Uuid) -> SshTunnelProfile {
        let mut s = SshTunnelProfile::new(
            "test-ssh",
            datatab_core::SshTunnelConfig {
                host: "bastion.example.com".to_string(),
                port: 22,
                user: "ec2-user".to_string(),
                auth_method: datatab_core::SshAuthMethod::Password,
                ssh_config_host: None,
            },
        );
        s.id = id;
        s
    }

    fn make_proxy(id: Uuid) -> ProxyProfile {
        ProxyProfile {
            id,
            name: "test-proxy".to_string(),
            kind: datatab_core::ProxyKind::Socks5,
            host: "proxy.example.com".to_string(),
            port: 1080,
            auth: datatab_core::ProxyAuth::None,
            no_proxy: None,
            enabled: true,
            save_secret: false,
        }
    }

    // -----------------------------------------------------------------------
    // AppFieldHintResolver tests
    // -----------------------------------------------------------------------

    #[test]
    fn hint_resolver_delegates_to_driver_without_branching_on_id() {
        let mut drivers: HashMap<String, Arc<dyn datatab_core::DbDriver>> = HashMap::new();
        drivers.insert(
            "fake".to_string(),
            Arc::new(FakeDriver::new_with_hint(ExportFieldHint::RequiredOnImport)),
        );

        let resolver = AppFieldHintResolver::new(drivers);
        let profile = make_profile("fake");
        let values = FormValues::default();

        let hint = resolver.hint(&profile, "secret_password", &values);
        assert_eq!(hint, ExportFieldHint::RequiredOnImport);
    }

    #[test]
    fn hint_resolver_returns_include_for_non_secret_fields() {
        let mut drivers: HashMap<String, Arc<dyn datatab_core::DbDriver>> = HashMap::new();
        drivers.insert("fake".to_string(), Arc::new(FakeDriver::new_default()));

        let resolver = AppFieldHintResolver::new(drivers);
        let profile = make_profile("fake");
        let values = FormValues::default();

        let hint = resolver.hint(&profile, "host", &values);
        assert_eq!(hint, ExportFieldHint::Include);
    }

    /// An unregistered RPC driver has no form that says which of its values
    /// are secret, so every value is hinted `Secret` (#411 ruling 2 item 3).
    /// So does a built-in config of an unknown driver id (#415).
    #[test]
    fn hint_resolver_fails_closed_for_an_unknown_external_driver() {
        let resolver = AppFieldHintResolver::new(HashMap::new());
        let profile = make_profile("totally-unknown-driver");
        let values = FormValues::default();

        let hint = resolver.hint(&profile, "secret_password", &values);
        assert_eq!(hint, ExportFieldHint::Secret);

        let mut built_in = ConnectionProfile::new("test", DbConfig::default_postgres());
        built_in.driver_id = Some("totally-unknown-driver".to_string());
        assert_eq!(
            resolver.hint(&built_in, "host", &values),
            ExportFieldHint::Secret
        );
    }

    /// An RPC profile with no driver id has no known form either, so every
    /// value is hinted `Secret` (#411 triage round 2, point 4). A built-in
    /// config with no driver id and no registered driver does too (#415).
    #[test]
    fn hint_resolver_fails_closed_for_an_external_profile_without_a_driver_id() {
        let resolver = AppFieldHintResolver::new(HashMap::new());
        let mut profile = make_profile("fake");
        profile.driver_id = None;
        let values = FormValues::default();

        let hint = resolver.hint(&profile, "secret_password", &values);
        assert_eq!(hint, ExportFieldHint::Secret);

        let mut built_in = ConnectionProfile::new("test", DbConfig::default_postgres());
        built_in.driver_id = None;
        assert_eq!(
            resolver.hint(&built_in, "host", &values),
            ExportFieldHint::Secret
        );
    }

    /// A profile whose form is unknown exports no literal value ref: not a
    /// built-in one whose driver is not registered, and not one without a
    /// driver id (#415).
    #[test]
    fn export_of_a_profile_with_an_unknown_form_carries_no_literal_value() {
        use datatab_portability::{EncryptionChoice, IncludeExclude};

        let mut unregistered = ConnectionProfile::new("a", DbConfig::default_postgres());
        unregistered.driver_id = Some("totally-unknown-driver".to_string());
        let mut without_id = ConnectionProfile::new("b", DbConfig::default_postgres());
        without_id.driver_id = None;
        for mut profile in [unregistered, without_id] {
            profile.value_refs.insert(
                "password".into(),
                datatab_core::ValueRef::literal("sentinel-unknown-form-3d71"),
            );
            let inputs = ExportInputs {
                connections_with_values: vec![(profile, FormValues::default())],
                auth_profiles: vec![],
                ssh_tunnels: vec![],
                proxies: vec![],
            };
            let graph = build_export_graph(&inputs);
            let opts = datatab_portability::ExportOptions {
                include_hooks: false,
                include_settings_overrides: false,
                embed_ssh_keys: false,
                encryption: EncryptionChoice::Plaintext { forced: true },
                connection_password: IncludeExclude::Include,
                proxy_credentials: IncludeExclude::Exclude,
                ssh_password: IncludeExclude::Exclude,
                auth_modes: Default::default(),
                per_secret_overrides: Default::default(),
            };
            let (bytes, report) = datatab_portability::export::export(
                &graph,
                &opts,
                &AppFieldHintResolver::new(HashMap::new()),
                &AppExportTransformResolver::new(HashMap::new()),
                &AppSecretReader::new(make_store_arc(FakeSecretStore::empty())),
            )
            .expect("export");

            let text = String::from_utf8(bytes).expect("utf8");
            assert!(!text.contains("sentinel-unknown-form-3d71"), "{text}");
            assert_eq!(report.required_ref_count, 1);
        }
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn export_hint_uses_registered_builtin_when_driver_id_is_none() {
        use datatab_core::ValueRef;
        use datatab_portability::{EncryptionChoice, IncludeExclude};

        let driver: Arc<dyn DbDriver> = Arc::new(datatab_driver_postgres::PostgresDriver::new());
        let drivers = HashMap::from([(driver.metadata().id.to_string(), driver.clone())]);
        let mut profile = ConnectionProfile::new("builtin", DbConfig::default_postgres());
        profile.driver_id = None;
        profile
            .value_refs
            .insert("password".into(), ValueRef::literal("literal-secret-433"));
        profile
            .value_refs
            .insert("user".into(), ValueRef::literal("ordinary-user-433"));
        let mut values = driver.extract_values(&profile.config);
        values.insert("host".into(), "ordinary-host-433".into());
        let hints = AppFieldHintResolver::new(drivers.clone());
        assert_eq!(
            hints.hint(&profile, "host", &values),
            ExportFieldHint::Include
        );
        assert_eq!(
            hints.hint(&profile, "password", &values),
            ExportFieldHint::Secret
        );
        let inputs = ExportInputs {
            connections_with_values: vec![(profile, values)],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
        };
        let graph = build_export_graph(&inputs);
        let options = datatab_portability::ExportOptions {
            include_hooks: false,
            include_settings_overrides: false,
            embed_ssh_keys: false,
            encryption: EncryptionChoice::Plaintext { forced: true },
            connection_password: IncludeExclude::Include,
            proxy_credentials: IncludeExclude::Exclude,
            ssh_password: IncludeExclude::Exclude,
            auth_modes: Default::default(),
            per_secret_overrides: Default::default(),
        };
        let (bytes, report) = datatab_portability::export::export(
            &graph,
            &options,
            &hints,
            &AppExportTransformResolver::new(drivers),
            &AppSecretReader::new(make_store_arc(FakeSecretStore::empty())),
        )
        .expect("export");
        assert!(!String::from_utf8_lossy(&bytes).contains("literal-secret-433"));
        let parsed = datatab_portability::import::parse(&bytes).expect("parse export");
        let connection = &parsed.bundle.connections[0];
        assert_eq!(
            connection.fields.get("host").map(String::as_str),
            Some("ordinary-host-433")
        );
        assert!(!connection.value_refs.contains_key("password"));
        assert!(connection.value_refs.contains_key("user"));
        assert_eq!(report.required_ref_count, 1);
        assert_eq!(connection.required_refs.len(), 1);
        assert_eq!(connection.required_refs[0].field, "password");
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn external_uri_without_a_registered_driver_exports_only_a_required_ref() {
        use datatab_portability::{EncryptionChoice, IncludeExclude};

        let driver: Arc<dyn DbDriver> = Arc::new(datatab_driver_postgres::PostgresDriver::new());
        let drivers = HashMap::from([(driver.metadata().id.to_string(), driver)]);
        let values = FormValues::from([
            ("use_uri".into(), "true".into()),
            (
                "uri".into(),
                "postgres://reader:uri-secret-433@localhost/example".into(),
            ),
        ]);
        for driver_id in [None, Some("unregistered".to_string())] {
            let mut profile = ConnectionProfile::new(
                "external URI",
                DbConfig::External {
                    kind: DbKind::Postgres,
                    values: values.clone(),
                },
            );
            profile.driver_id = driver_id;
            let transforms = AppExportTransformResolver::new(drivers.clone());
            let transform_is_none = matches!(
                transforms.transform(&profile, "uri", &values),
                FieldExportTransform::None
            );
            let inputs = ExportInputs {
                connections_with_values: vec![(profile, values.clone())],
                ..Default::default()
            };
            let options = datatab_portability::ExportOptions {
                include_hooks: false,
                include_settings_overrides: false,
                embed_ssh_keys: false,
                encryption: EncryptionChoice::Plaintext { forced: true },
                connection_password: IncludeExclude::Include,
                proxy_credentials: IncludeExclude::Exclude,
                ssh_password: IncludeExclude::Exclude,
                auth_modes: Default::default(),
                per_secret_overrides: Default::default(),
            };
            let (bytes, report) = datatab_portability::export::export(
                &build_export_graph(&inputs),
                &options,
                &AppFieldHintResolver::new(drivers.clone()),
                &transforms,
                &AppSecretReader::new(make_store_arc(FakeSecretStore::empty())),
            )
            .expect("export");
            let parsed = datatab_portability::import::parse(&bytes).expect("parse export");
            let connection = &parsed.bundle.connections[0];
            assert_eq!(
                (transform_is_none, connection.fields.contains_key("uri")),
                (true, false),
                "an unknown external form must not transform or export a URI value"
            );
            assert!(!String::from_utf8_lossy(&bytes).contains("uri-secret-433"));
            assert!(!connection.value_refs.contains_key("uri"));
            assert_eq!(report.required_ref_count, 2);
            assert_eq!(connection.required_refs.len(), 2);
            assert!(
                connection
                    .required_refs
                    .iter()
                    .any(|reference| reference.field == "uri")
            );
        }
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn uri_transform_uses_registered_builtin_when_driver_id_is_none() {
        let driver: Arc<dyn DbDriver> = Arc::new(datatab_driver_postgres::PostgresDriver::new());
        let drivers = HashMap::from([(driver.metadata().id.to_string(), driver.clone())]);
        let values = FormValues::from([
            ("use_uri".into(), "true".into()),
            (
                "uri".into(),
                "postgres://reader:uri-secret-433@localhost/example".into(),
            ),
        ]);
        let config = driver.build_config(&values).expect("URI config");
        let mut profile = ConnectionProfile::new("builtin URI", config);
        profile.driver_id = None;
        let resolver = AppExportTransformResolver::new(drivers);

        let transform = resolver.transform(&profile, "uri", &values);

        let FieldExportTransform::SplitSecret { skeleton, secret } = transform else {
            panic!("a registered builtin without a stored driver id must split URI credentials");
        };
        assert_eq!(skeleton, "postgres://reader:@localhost/example");
        assert_eq!(secret.expose_secret(), "uri-secret-433");
        assert!(matches!(
            resolver.transform(&profile, "host", &values),
            FieldExportTransform::None
        ));
    }

    #[test]
    fn hint_resolver_dispatches_independently_per_driver() {
        // Two different drivers, each with a distinct export hint behavior.
        // This verifies the resolver calls the driver generically and does not
        // branch on driver_id strings.
        let mut drivers: HashMap<String, Arc<dyn datatab_core::DbDriver>> = HashMap::new();
        drivers.insert(
            "driver-a".to_string(),
            Arc::new(FakeDriver::new_with_hint(ExportFieldHint::RequiredOnImport)),
        );
        drivers.insert(
            "driver-b".to_string(),
            Arc::new(FakeDriver::new_with_hint(ExportFieldHint::Secret)),
        );

        let resolver = AppFieldHintResolver::new(drivers);
        let values = FormValues::default();

        let hint_a = resolver.hint(&make_profile("driver-a"), "secret_key", &values);
        let hint_b = resolver.hint(&make_profile("driver-b"), "secret_key", &values);

        assert_eq!(hint_a, ExportFieldHint::RequiredOnImport);
        assert_eq!(hint_b, ExportFieldHint::Secret);
    }

    // -----------------------------------------------------------------------
    // AppSecretReader tests
    // -----------------------------------------------------------------------

    #[test]
    fn secret_reader_returns_value_for_known_ref() {
        use datatab_core::secrecy::ExposeSecret;

        let store = FakeSecretStore::with(&[("datatab/conn/profile-1/password", "s3cr3t")]);
        let reader = AppSecretReader::new(make_store_arc(store));

        let result = reader.read("datatab/conn/profile-1/password");
        assert!(result.is_some());
        assert_eq!(result.unwrap().expose_secret(), "s3cr3t");
    }

    #[test]
    fn secret_reader_returns_none_for_missing_ref() {
        let store = FakeSecretStore::empty();
        let reader = AppSecretReader::new(make_store_arc(store));

        let result = reader.read("datatab/conn/nonexistent/password");
        assert!(result.is_none());
    }

    #[test]
    fn secret_reader_never_panics_on_poisoned_lock() {
        // Poison the RwLock by panicking inside a write guard from another thread.
        let arc: Arc<RwLock<Box<dyn SecretStore>>> = make_store_arc(FakeSecretStore::empty());
        let arc2 = arc.clone();

        let _ = std::thread::spawn(move || {
            let _guard = arc2.write().unwrap();
            panic!("poisoning the lock intentionally");
        })
        .join();

        // After poisoning, AppSecretReader must return None rather than panic.
        let reader = AppSecretReader { store: arc };
        let result = reader.read("any/key");
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // build_export_graph tests
    // -----------------------------------------------------------------------

    #[test]
    fn build_export_graph_includes_all_provided_entities() {
        let conn_id = Uuid::new_v4();
        let auth_id = Uuid::new_v4();
        let ssh_id = Uuid::new_v4();
        let proxy_id = Uuid::new_v4();

        let mut profile = make_profile("fake");
        profile.id = conn_id;

        let inputs = ExportInputs {
            connections_with_values: vec![(profile, FormValues::default())],
            auth_profiles: vec![make_auth_profile(auth_id)],
            ssh_tunnels: vec![make_ssh_tunnel(ssh_id)],
            proxies: vec![make_proxy(proxy_id)],
        };

        let graph = build_export_graph(&inputs);

        assert_eq!(graph.connections.len(), 1);
        assert_eq!(graph.connections[0].profile.id, conn_id);

        assert_eq!(graph.auth_profiles.len(), 1);
        assert_eq!(graph.auth_profiles[0].id, auth_id);

        assert_eq!(graph.ssh_tunnels.len(), 1);
        assert_eq!(graph.ssh_tunnels[0].id, ssh_id);

        assert_eq!(graph.proxies.len(), 1);
        assert_eq!(graph.proxies[0].id, proxy_id);
    }

    #[test]
    fn build_export_graph_with_no_side_entities() {
        let mut profile = make_profile("fake");
        let conn_id = Uuid::new_v4();
        profile.id = conn_id;

        let inputs = ExportInputs {
            connections_with_values: vec![(profile, FormValues::default())],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
        };

        let graph = build_export_graph(&inputs);

        assert_eq!(graph.connections.len(), 1);
        assert_eq!(graph.auth_profiles.len(), 0);
        assert_eq!(graph.ssh_tunnels.len(), 0);
        assert_eq!(graph.proxies.len(), 0);
    }

    #[test]
    fn build_export_graph_with_empty_inputs_is_empty() {
        let inputs = ExportInputs {
            connections_with_values: vec![],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
        };

        let graph = build_export_graph(&inputs);

        assert_eq!(graph.connections.len(), 0);
        assert_eq!(graph.auth_profiles.len(), 0);
        assert_eq!(graph.ssh_tunnels.len(), 0);
        assert_eq!(graph.proxies.len(), 0);
    }

    #[test]
    fn build_export_graph_only_includes_explicitly_passed_entities() {
        // The function does NOT auto-discover transitive references — that
        // responsibility belongs to the UI layer before calling build_export_graph.
        // Only the entities present in `inputs` appear in the graph.
        let conn_id = Uuid::new_v4();
        let included_auth = Uuid::new_v4();
        let excluded_auth = Uuid::new_v4();

        let mut profile = make_profile("fake");
        profile.id = conn_id;

        let inputs = ExportInputs {
            connections_with_values: vec![(profile, FormValues::default())],
            auth_profiles: vec![make_auth_profile(included_auth)],
            ssh_tunnels: vec![],
            proxies: vec![],
        };

        let graph = build_export_graph(&inputs);

        assert_eq!(graph.auth_profiles.len(), 1);
        assert_eq!(graph.auth_profiles[0].id, included_auth);
        assert!(!graph.auth_profiles.iter().any(|a| a.id == excluded_auth));
    }

    // -----------------------------------------------------------------------
    // ImportPersistence tests (T5.1 — import orchestration, TDD first)
    // -----------------------------------------------------------------------

    use super::{ImportPersistence, persist_import_actions};
    use datatab_portability::ImportActions;

    /// Minimal fake implementation of `ImportPersistence` for unit tests.
    struct FakePersistence {
        drivers: std::collections::HashSet<String>,
        auth_count: usize,
        ssh_count: usize,
        proxy_count: usize,
        conn_names: Vec<String>,
        /// `None` keys succeed; `Some(false)` simulates keyring-locked failure.
        secret_outcomes: HashMap<String, bool>,
        /// When set, every `write_secret` call fails regardless of the ref,
        /// for tests where the exact ref cannot be known ahead of time (e.g.
        /// a freshly generated connection id).
        fail_all_secrets: bool,
        /// The secret-kind fields every connection's driver declares.
        secret_fields: Vec<String>,
        /// How many value refs each added connection still carried.
        added_value_ref_counts: Vec<usize>,
    }

    impl FakePersistence {
        fn with_drivers(drivers: &[&str]) -> Self {
            Self {
                drivers: drivers.iter().map(|s| s.to_string()).collect(),
                auth_count: 0,
                ssh_count: 0,
                proxy_count: 0,
                conn_names: Vec::new(),
                secret_outcomes: HashMap::new(),
                fail_all_secrets: false,
                secret_fields: Vec::new(),
                added_value_ref_counts: Vec::new(),
            }
        }

        fn all_drivers() -> Self {
            let mut s = Self::with_drivers(&[]);
            s.drivers.insert("*".to_string());
            s
        }

        fn with_keyring_failure(mut self, secret_ref: &str) -> Self {
            self.secret_outcomes.insert(secret_ref.to_string(), false);
            self
        }

        fn with_all_secrets_failing(mut self) -> Self {
            self.fail_all_secrets = true;
            self
        }
    }

    impl ImportPersistence for FakePersistence {
        fn add_auth_profile(&mut self, _profile: AuthProfile) {
            self.auth_count += 1;
        }

        fn add_ssh_tunnel(&mut self, _tunnel: SshTunnelProfile) {
            self.ssh_count += 1;
        }

        fn add_proxy(&mut self, _proxy: ProxyProfile) {
            self.proxy_count += 1;
        }

        fn add_connection(&mut self, profile: ConnectionProfile) -> ConnectionInsertResult {
            let driver_id = profile.driver_id().to_string();
            if !self.drivers.contains("*") && !self.drivers.contains(&driver_id) {
                return ConnectionInsertResult::NeedsDriver;
            }
            self.conn_names.push(profile.name.clone());
            self.added_value_ref_counts.push(profile.value_refs.len());
            ConnectionInsertResult::Ok
        }

        fn secret_field_ids(&self, _profile: &ConnectionProfile) -> Vec<String> {
            self.secret_fields.clone()
        }

        fn write_secret(&self, secret_ref: &str, _secret: &SecretString) -> bool {
            if self.fail_all_secrets {
                return false;
            }
            self.secret_outcomes
                .get(secret_ref)
                .copied()
                .unwrap_or(true)
        }

        fn chosen_auth_profile_values(
            &mut self,
            _connection_id: uuid::Uuid,
            _driver_id: &str,
            _auth_id: uuid::Uuid,
        ) -> Option<FormValues> {
            Some(FormValues::new())
        }

        fn hydrate_auth_secret_fields(
            &mut self,
            _auth_id: uuid::Uuid,
            _fields: HashMap<String, SecretString>,
        ) {
            // No-op for tests that don't verify hydration.
        }
    }

    fn make_import_actions_empty() -> ImportActions {
        ImportActions {
            connections: vec![],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        }
    }

    fn make_conn_profile(name: &str, driver_id: &str) -> ConnectionProfile {
        let mut p = ConnectionProfile::new(
            name,
            datatab_core::DbConfig::External {
                kind: datatab_core::DbKind::SQLite,
                values: FormValues::default(),
            },
        );
        p.driver_id = Some(driver_id.to_string());
        p
    }

    /// A secret supplied for a connection's required reference, with the
    /// default `keyring_secret_field_ids`, goes to the connection password
    /// slot of an inserted connection and is not written for a skipped one
    /// (#411 triage round 2, point 3).
    #[test]
    fn persist_routes_a_supplied_secret_to_the_password_slot_by_default() {
        let inserted = make_conn_profile("Inserted", "postgres");
        let skipped = make_conn_profile("Skipped", "unregistered");
        let inserted_ref = datatab_core::connection_secret_ref(&inserted.id);
        let skipped_ref = datatab_core::connection_secret_ref(&skipped.id);
        // A failing write is recorded, which shows whether it was attempted.
        let mut deps = FakePersistence::with_drivers(&["postgres"])
            .with_keyring_failure(&inserted_ref)
            .with_keyring_failure(&skipped_ref);
        let mut actions = make_import_actions_empty();
        actions.connection_field_secrets = vec![
            (
                inserted.id,
                "password".to_string(),
                SecretString::from("supplied"),
            ),
            (
                skipped.id,
                "password".to_string(),
                SecretString::from("supplied"),
            ),
        ];
        actions.connections = vec![inserted, skipped];

        let outcome = persist_import_actions(actions, &mut deps);

        let attempted: Vec<&str> = outcome
            .secret_failures
            .iter()
            .map(|(_, secret_ref)| secret_ref.as_str())
            .collect();
        assert_eq!(attempted, [inserted_ref.as_str()]);
    }

    /// A literal value ref on the password in a bundle is the password: it is
    /// taken out of the profile and written to the password slot (#415).
    #[test]
    fn persist_moves_a_literal_password_ref_to_the_password_slot() {
        let mut profile = make_conn_profile("Literal", "postgres");
        profile.value_refs.insert(
            "password".into(),
            datatab_core::ValueRef::literal("sentinel"),
        );
        profile
            .value_refs
            .insert("host".into(), datatab_core::ValueRef::literal("db.example"));
        let password_ref = datatab_core::connection_secret_ref(&profile.id);
        let mut deps =
            FakePersistence::with_drivers(&["postgres"]).with_keyring_failure(&password_ref);
        deps.secret_fields = vec!["password".to_string()];
        let mut actions = make_import_actions_empty();
        actions.connections = vec![profile];

        let outcome = persist_import_actions(actions, &mut deps);

        // The failing write shows the password slot was written.
        assert_eq!(outcome.secret_failures.len(), 1);
        assert_eq!(deps.added_value_ref_counts, [1]);
    }

    /// The user chose not to save the password: the literal is dropped.
    #[test]
    fn persist_drops_a_literal_password_ref_when_the_password_is_not_saved() {
        let mut profile = make_conn_profile("Literal", "postgres");
        profile.save_password = false;
        profile.value_refs.insert(
            "password".into(),
            datatab_core::ValueRef::literal("sentinel"),
        );
        let password_ref = datatab_core::connection_secret_ref(&profile.id);
        let mut deps =
            FakePersistence::with_drivers(&["postgres"]).with_keyring_failure(&password_ref);
        deps.secret_fields = vec!["password".to_string()];
        let mut actions = make_import_actions_empty();
        actions.connections = vec![profile];

        let outcome = persist_import_actions(actions, &mut deps);

        assert!(outcome.secret_failures.is_empty());
        assert_eq!(deps.added_value_ref_counts, [0]);
    }

    #[test]
    fn persist_empty_actions_returns_empty_outcome() {
        let mut deps = FakePersistence::all_drivers();
        let outcome = persist_import_actions(make_import_actions_empty(), &mut deps);

        assert!(outcome.succeeded.is_empty());
        assert!(outcome.secret_failures.is_empty());
        assert!(outcome.needs_driver.is_empty());
    }

    #[test]
    fn persist_auth_ssh_proxy_are_inserted_before_connections() {
        let mut deps = FakePersistence::all_drivers();

        let auth = AuthProfile {
            id: Uuid::new_v4(),
            name: "TestAuth".to_string(),
            provider_id: "test-provider".to_string(),
            fields: HashMap::new(),
            secret_fields: HashMap::new(),
            enabled: true,
        };
        let ssh = SshTunnelProfile::new(
            "TestSSH",
            datatab_core::SshTunnelConfig {
                host: "bastion.example.com".to_string(),
                port: 22,
                user: "ec2-user".to_string(),
                auth_method: datatab_core::SshAuthMethod::Password,
                ssh_config_host: None,
            },
        );
        let proxy = ProxyProfile {
            id: Uuid::new_v4(),
            name: "TestProxy".to_string(),
            kind: datatab_core::ProxyKind::Http,
            host: "proxy.example.com".to_string(),
            port: 3128,
            auth: datatab_core::ProxyAuth::None,
            no_proxy: None,
            enabled: true,
            save_secret: false,
        };
        let conn = make_conn_profile("TestConn", "postgres");

        let actions = ImportActions {
            connections: vec![conn],
            auth_profiles: vec![auth],
            ssh_tunnels: vec![ssh],
            proxies: vec![proxy],
            secret_writes: vec![],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        assert_eq!(deps.auth_count, 1);
        assert_eq!(deps.ssh_count, 1);
        assert_eq!(deps.proxy_count, 1);
        assert_eq!(deps.conn_names.len(), 1);
        assert_eq!(deps.conn_names[0], "TestConn");
        assert!(outcome.needs_driver.is_empty());
        assert_eq!(outcome.succeeded.len(), 4);
    }

    #[test]
    fn persist_unknown_driver_skips_connection_and_records_needs_driver() {
        let mut deps = FakePersistence::with_drivers(&["postgres"]);
        let conn_unknown = make_conn_profile("UnknownConn", "totally-unknown-driver");

        let actions = ImportActions {
            connections: vec![conn_unknown],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        assert_eq!(outcome.needs_driver.len(), 1);
        assert_eq!(outcome.needs_driver[0].0, "UnknownConn");
        assert_eq!(outcome.needs_driver[0].1, "totally-unknown-driver");
        assert!(outcome.succeeded.is_empty());
        assert!(deps.conn_names.is_empty());
    }

    #[test]
    fn persist_known_driver_inserts_connection_into_succeeded() {
        let mut deps = FakePersistence::with_drivers(&["postgres"]);
        let conn = make_conn_profile("ProdPG", "postgres");

        let actions = ImportActions {
            connections: vec![conn],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        assert_eq!(outcome.succeeded.len(), 1);
        assert_eq!(outcome.succeeded[0], "ProdPG");
        assert!(outcome.needs_driver.is_empty());
    }

    #[test]
    fn persist_secret_write_failure_is_recorded_not_silently_lost() {
        let mut deps =
            FakePersistence::all_drivers().with_keyring_failure("datatab:conn:aaaa-bbbb:password");

        let actions = ImportActions {
            connections: vec![],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![(
                "datatab:conn:aaaa-bbbb:password".to_string(),
                SecretString::from("s3cr3t".to_string()),
            )],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        assert_eq!(outcome.secret_failures.len(), 1);
        assert_eq!(
            outcome.secret_failures[0].1,
            "datatab:conn:aaaa-bbbb:password"
        );
    }

    #[test]
    fn persist_successful_secret_write_does_not_appear_in_failures() {
        let mut deps = FakePersistence::all_drivers();

        let actions = ImportActions {
            connections: vec![],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![(
                "datatab:conn:1111-2222:password".to_string(),
                SecretString::from("ok".to_string()),
            )],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        assert!(outcome.secret_failures.is_empty());
    }

    #[test]
    fn persist_reports_unreadable_value_refs_as_config_failures_naming_the_fields() {
        let mut deps = FakePersistence::all_drivers();
        let mut actions = make_import_actions_empty();
        actions
            .connections
            .push(make_conn_profile("ProdPG", "postgres"));
        actions.value_ref_failures.push((
            "Removed source".to_string(),
            vec!["host".to_string(), "password".to_string()],
        ));

        let outcome = persist_import_actions(actions, &mut deps);

        assert_eq!(
            outcome.config_failures,
            [(
                "Removed source".to_string(),
                "unreadable value reference for: host, password".to_string()
            )]
        );
        assert_eq!(outcome.succeeded, ["ProdPG"]);
    }

    #[test]
    fn persist_reuse_does_not_insert_new_entity_and_unknown_driver_does_not_block_others() {
        let mut deps = FakePersistence::with_drivers(&["postgres"]);

        let conn_known = make_conn_profile("KnownConn", "postgres");
        let conn_unknown = make_conn_profile("UnknownConn", "some-external-driver");

        let actions = ImportActions {
            connections: vec![conn_known, conn_unknown],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        assert_eq!(outcome.succeeded.len(), 1);
        assert_eq!(outcome.needs_driver.len(), 1);
        assert_eq!(deps.conn_names.len(), 1);
        assert_eq!(deps.conn_names[0], "KnownConn");
    }

    /// `persist_import_actions` must carry the `DbConfig::External` variant
    /// (carrying form values) to `add_connection` so the app-layer rebuild can
    /// call `build_config(values)` with the real driver rather than
    /// `extract_values(placeholder_config)`.
    ///
    /// This test verifies that a connection profile emitted by the portability
    /// crate (with `DbConfig::External`) arrives at the persistence seam with
    /// `driver_id()` intact — not silently rewritten to `"postgres"`.
    #[test]
    fn persist_connection_driver_id_is_preserved_through_seam() {
        struct DriverIdRecorder {
            recorded_driver_ids: Vec<String>,
            recorded_config_is_external: Vec<bool>,
        }

        impl ImportPersistence for DriverIdRecorder {
            fn add_auth_profile(&mut self, _: AuthProfile) {}
            fn add_ssh_tunnel(&mut self, _: SshTunnelProfile) {}
            fn add_proxy(&mut self, _: ProxyProfile) {}

            fn add_connection(&mut self, profile: ConnectionProfile) -> ConnectionInsertResult {
                self.recorded_driver_ids
                    .push(profile.driver_id().to_string());
                let is_external = matches!(profile.config, datatab_core::DbConfig::External { .. });
                self.recorded_config_is_external.push(is_external);
                ConnectionInsertResult::Ok
            }

            fn write_secret(&self, _: &str, _: &SecretString) -> bool {
                true
            }

            fn chosen_auth_profile_values(
                &mut self,
                _connection_id: uuid::Uuid,
                _driver_id: &str,
                _auth_id: uuid::Uuid,
            ) -> Option<FormValues> {
                Some(FormValues::new())
            }

            fn hydrate_auth_secret_fields(
                &mut self,
                _auth_id: uuid::Uuid,
                _fields: HashMap<String, SecretString>,
            ) {
            }
        }

        let mut recorder = DriverIdRecorder {
            recorded_driver_ids: Vec::new(),
            recorded_config_is_external: Vec::new(),
        };

        let mut mysql_profile = ConnectionProfile::new(
            "MySQL Prod",
            datatab_core::DbConfig::External {
                kind: datatab_core::DbKind::SQLite,
                values: {
                    let mut v = FormValues::default();
                    v.insert("host".to_string(), "mysql.example.com".to_string());
                    v.insert("port".to_string(), "3306".to_string());
                    v
                },
            },
        );
        mysql_profile.driver_id = Some("mysql".to_string());

        let actions = ImportActions {
            connections: vec![mysql_profile],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        persist_import_actions(actions, &mut recorder);

        assert_eq!(recorder.recorded_driver_ids.len(), 1);
        assert_eq!(
            recorder.recorded_driver_ids[0], "mysql",
            "driver_id must arrive at the persistence seam as 'mysql', \
             not rewritten to 'postgres'"
        );
        assert!(
            recorder.recorded_config_is_external[0],
            "config must be DbConfig::External so the app layer can call \
             build_config(values) with the real driver"
        );
    }

    // -----------------------------------------------------------------------
    // Finding #4 — build_config failure is recorded, not silently persisted
    // -----------------------------------------------------------------------

    /// A persistence implementation that reports ConfigFailed for the "bad-driver" id.
    struct FailingConfigPersistence {
        inserted_names: Vec<String>,
    }

    impl ImportPersistence for FailingConfigPersistence {
        fn add_auth_profile(&mut self, _: AuthProfile) {}
        fn add_ssh_tunnel(&mut self, _: SshTunnelProfile) {}
        fn add_proxy(&mut self, _: ProxyProfile) {}

        fn add_connection(&mut self, profile: ConnectionProfile) -> ConnectionInsertResult {
            let driver_id = profile.driver_id().to_string();
            if driver_id == "bad-driver" {
                return ConnectionInsertResult::ConfigFailed(
                    "required field 'host' is missing".to_string(),
                );
            }
            self.inserted_names.push(profile.name.clone());
            ConnectionInsertResult::Ok
        }

        fn write_secret(&self, _: &str, _: &SecretString) -> bool {
            true
        }

        fn chosen_auth_profile_values(
            &mut self,
            _connection_id: uuid::Uuid,
            _driver_id: &str,
            _auth_id: uuid::Uuid,
        ) -> Option<FormValues> {
            Some(FormValues::new())
        }

        fn hydrate_auth_secret_fields(
            &mut self,
            _auth_id: uuid::Uuid,
            _fields: HashMap<String, SecretString>,
        ) {
        }
    }

    #[test]
    fn persist_config_failure_is_not_persisted_and_not_counted_as_succeeded() {
        let mut deps = FailingConfigPersistence {
            inserted_names: Vec::new(),
        };

        let mut profile = make_conn_profile("BadConfig", "bad-driver");
        profile.id = Uuid::parse_str("aaaaaaaa-0000-0000-0000-000000000001").unwrap();

        let actions = ImportActions {
            connections: vec![profile],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        assert!(
            outcome.succeeded.is_empty(),
            "config-failure connection must NOT appear in succeeded"
        );
        assert!(
            outcome.needs_driver.is_empty(),
            "config-failure must NOT appear in needs_driver"
        );
        assert_eq!(
            outcome.config_failures.len(),
            1,
            "config-failure must be recorded in config_failures"
        );
        assert_eq!(outcome.config_failures[0].0, "BadConfig");
        assert!(deps.inserted_names.is_empty(), "must NOT be persisted");
    }

    // -----------------------------------------------------------------------
    // Finding #8 — skipped connection secrets are not written to keyring
    // -----------------------------------------------------------------------

    /// Mutable-tracking version for this test.
    struct TrackingPersistence {
        registered: std::collections::HashSet<String>,
        written_secret_refs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ImportPersistence for TrackingPersistence {
        fn add_auth_profile(&mut self, _: AuthProfile) {}
        fn add_ssh_tunnel(&mut self, _: SshTunnelProfile) {}
        fn add_proxy(&mut self, _: ProxyProfile) {}

        fn add_connection(&mut self, profile: ConnectionProfile) -> ConnectionInsertResult {
            if !self.registered.contains(&profile.driver_id().to_string()) {
                ConnectionInsertResult::NeedsDriver
            } else {
                ConnectionInsertResult::Ok
            }
        }

        fn write_secret(&self, secret_ref: &str, _: &SecretString) -> bool {
            self.written_secret_refs
                .lock()
                .unwrap()
                .push(secret_ref.to_string());
            true
        }

        fn chosen_auth_profile_values(
            &mut self,
            _connection_id: uuid::Uuid,
            _driver_id: &str,
            _auth_id: uuid::Uuid,
        ) -> Option<FormValues> {
            Some(FormValues::new())
        }

        fn hydrate_auth_secret_fields(
            &mut self,
            _auth_id: uuid::Uuid,
            _fields: HashMap<String, SecretString>,
        ) {
        }
    }

    #[test]
    fn persist_skipped_connection_secrets_are_not_written() {
        use datatab_core::connection_secret_ref;

        let skipped_id = Uuid::parse_str("bbbbbbbb-0000-0000-0000-000000000002").unwrap();
        let ok_id = Uuid::parse_str("cccccccc-0000-0000-0000-000000000003").unwrap();

        let mut skipped_conn = make_conn_profile("SkippedConn", "unknown-driver");
        skipped_conn.id = skipped_id;

        let mut ok_conn = make_conn_profile("OkConn", "postgres");
        ok_conn.id = ok_id;

        let skipped_secret_ref = connection_secret_ref(&skipped_id);
        let ok_secret_ref = connection_secret_ref(&ok_id);

        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut deps = TrackingPersistence {
            registered: ["postgres".to_string()].into_iter().collect(),
            written_secret_refs: written.clone(),
        };

        let actions = ImportActions {
            connections: vec![skipped_conn, ok_conn],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![
                (
                    skipped_secret_ref.clone(),
                    SecretString::from("skipped-secret".to_string()),
                ),
                (
                    ok_secret_ref.clone(),
                    SecretString::from("ok-secret".to_string()),
                ),
            ],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let outcome = persist_import_actions(actions, &mut deps);

        let written_refs = written.lock().unwrap();

        assert!(
            !written_refs.contains(&skipped_secret_ref),
            "secret for skipped connection must NOT be written to keyring"
        );
        assert!(
            written_refs.contains(&ok_secret_ref),
            "secret for successfully persisted connection must be written to keyring"
        );
        assert_eq!(outcome.needs_driver.len(), 1);
        assert_eq!(outcome.succeeded.len(), 1);
    }

    // --- Phase 2.8: ExportReport::skipped_connections for unregistered drivers (R-ROB-2 / M5) ---

    #[test]
    fn export_report_skipped_connections_can_be_populated() {
        let mut report = datatab_portability::ExportReport::default();

        report
            .skipped_connections
            .push(("Prod DB".to_string(), "unknown-driver".to_string()));

        assert_eq!(
            report.skipped_connections.len(),
            1,
            "skipped_connections must be recordable"
        );
        assert_eq!(report.skipped_connections[0].0, "Prod DB");
        assert_eq!(report.skipped_connections[0].1, "unknown-driver");
        assert!(
            report.warnings.is_empty(),
            "skipped connection must not also appear as a warning unless explicitly added"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 4.1 — mapto_candidates (M7 / R-WIZ-3)
    // -----------------------------------------------------------------------

    #[test]
    fn mapto_candidates_auth_profile_returns_all_dest_auth_profiles() {
        let auth1 = make_auth_profile(Uuid::new_v4());
        let auth2 = make_auth_profile(Uuid::new_v4());

        let dest = OwnedDestSnapshot {
            auth_profiles: vec![auth1.clone(), auth2.clone()],
            ssh_tunnels: vec![],
            proxies: vec![],
            connections: vec![],
        };

        let candidates =
            super::mapto_candidates(datatab_portability::ConflictKind::AuthProfile, &dest);

        assert_eq!(candidates.len(), 2, "must return all auth profiles");
        let ids: Vec<_> = candidates.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&auth1.id));
        assert!(ids.contains(&auth2.id));
    }

    #[test]
    fn mapto_candidates_ssh_tunnel_returns_all_dest_tunnels() {
        let ssh1 = make_ssh_tunnel(Uuid::new_v4());
        let ssh2 = make_ssh_tunnel(Uuid::new_v4());

        let dest = OwnedDestSnapshot {
            auth_profiles: vec![],
            ssh_tunnels: vec![ssh1.clone(), ssh2.clone()],
            proxies: vec![],
            connections: vec![],
        };

        let candidates =
            super::mapto_candidates(datatab_portability::ConflictKind::SshTunnel, &dest);

        assert_eq!(candidates.len(), 2);
        let ids: Vec<_> = candidates.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&ssh1.id));
        assert!(ids.contains(&ssh2.id));
    }

    #[test]
    fn mapto_candidates_empty_dest_returns_empty_list() {
        let dest = OwnedDestSnapshot {
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            connections: vec![],
        };

        let candidates =
            super::mapto_candidates(datatab_portability::ConflictKind::AuthProfile, &dest);
        assert!(
            candidates.is_empty(),
            "empty dest must yield empty candidate list"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 4.2 — ConfirmSummary (C3 / R-WIZ-1)
    // -----------------------------------------------------------------------

    #[test]
    fn confirm_summary_counts_bundle_entities() {
        use datatab_portability::ParsedBundle;
        use datatab_portability::bundle::{
            AuthEntry, Bundle, BundleMeta, CURRENT_FORMAT_VERSION, ConnectionEntry, EncryptionMode,
            SshAuthMethodKind, SshEntry,
        };

        let bundle = Bundle {
            bundle: BundleMeta {
                format_version: CURRENT_FORMAT_VERSION,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                datatab_version: "0.7.0-dev.0".to_string(),
                encryption: EncryptionMode::None,
            },
            drivers: vec![],
            connections: vec![
                ConnectionEntry {
                    local_id: "c1".to_string(),
                    name: "Conn1".to_string(),
                    driver_id: "postgres".to_string(),
                    fields: Default::default(),
                    local_path_fields: Default::default(),
                    required_refs: vec![],
                    auth_profile_local_id: None,
                    access: None,
                    value_refs: Default::default(),
                    include_hooks: false,
                    include_settings_overrides: false,
                    hooks_payload: None,
                    settings_overrides_payload: None,
                    kind: Some("Postgres".to_string()),
                    uri_secret_fields: vec![],
                },
                ConnectionEntry {
                    local_id: "c2".to_string(),
                    name: "Conn2".to_string(),
                    driver_id: "unknown-driver".to_string(),
                    fields: Default::default(),
                    local_path_fields: Default::default(),
                    required_refs: vec![],
                    auth_profile_local_id: None,
                    access: None,
                    value_refs: Default::default(),
                    include_hooks: false,
                    include_settings_overrides: false,
                    hooks_payload: None,
                    settings_overrides_payload: None,
                    kind: Some("MongoDB".to_string()),
                    uri_secret_fields: vec![],
                },
            ],
            auth_profiles: vec![AuthEntry {
                local_id: "a1".to_string(),
                name: "MySSO".to_string(),
                provider_id: "aws-sso".to_string(),
                enabled: true,
                fields: Default::default(),
                secret_field_names: vec![],
                required_refs: vec![],
            }],
            ssh_tunnels: vec![SshEntry {
                local_id: "s1".to_string(),
                name: "Bastion".to_string(),
                host: "bastion.example.com".to_string(),
                port: 22,
                user: "ec2-user".to_string(),
                ssh_config_host: None,
                auth_method: SshAuthMethodKind::Password,
                key_embedded: false,
                required_refs: vec![],
            }],
            proxies: vec![],
            secrets: None,
        };

        let parsed = ParsedBundle {
            bundle,
            decrypted_secrets: None,
        };

        let plan = datatab_portability::ImportPlan {
            conflicts: vec![],
            required_resolutions: vec![],
        };

        let registered: std::collections::HashSet<String> =
            ["postgres".to_string()].into_iter().collect();
        let summary = super::confirm_summary(&parsed, &plan, &registered);

        assert_eq!(summary.connection_count, 2);
        assert_eq!(summary.auth_profile_count, 1);
        assert_eq!(summary.ssh_tunnel_count, 1);
        assert_eq!(summary.proxy_count, 0);
        assert_eq!(summary.conflict_count, 0);
        assert_eq!(summary.required_resolution_count, 0);
        assert!(
            summary.has_driver_not_installed,
            "unknown-driver must trigger has_driver_not_installed"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 4.3 — H3: hydrate_auth_secret_fields (R-INT-2)
    // -----------------------------------------------------------------------

    #[test]
    fn hydrate_auth_secret_fields_is_called_for_new_auth_profiles() {
        // After persist_import_actions, the hydrate_auth_secret_fields seam must
        // be called for newly created auth profiles (H3 / ADR-4 / R-INT-2).
        struct HydrationTracker {
            hydrated: HashMap<uuid::Uuid, HashMap<String, SecretString>>,
        }

        impl ImportPersistence for HydrationTracker {
            fn add_auth_profile(&mut self, _p: AuthProfile) {}
            fn add_ssh_tunnel(&mut self, _: SshTunnelProfile) {}
            fn add_proxy(&mut self, _: ProxyProfile) {}
            fn add_connection(&mut self, _: ConnectionProfile) -> ConnectionInsertResult {
                ConnectionInsertResult::Ok
            }
            fn write_secret(&self, _: &str, _: &SecretString) -> bool {
                true
            }
            fn chosen_auth_profile_values(
                &mut self,
                _connection_id: uuid::Uuid,
                _driver_id: &str,
                _auth_id: uuid::Uuid,
            ) -> Option<FormValues> {
                Some(FormValues::new())
            }

            fn hydrate_auth_secret_fields(
                &mut self,
                auth_id: uuid::Uuid,
                fields: HashMap<String, SecretString>,
            ) {
                self.hydrated.insert(auth_id, fields);
            }
        }

        let auth_id = Uuid::new_v4();
        let auth_secret_ref = datatab_core::auth_field_secret_ref(&auth_id, "token");

        let auth = AuthProfile {
            id: auth_id,
            name: "SSO Profile".to_string(),
            provider_id: "aws-sso".to_string(),
            fields: HashMap::new(),
            secret_fields: HashMap::new(),
            enabled: true,
        };

        let actions = ImportActions {
            auth_profiles: vec![auth],
            connections: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secret_writes: vec![(
                auth_secret_ref.clone(),
                SecretString::from("hydrated-secret".to_string()),
            )],
            kind_failures: vec![],
            unresolved_ref_connections: vec![],
            value_ref_failures: vec![],
            chosen_auth_profiles: vec![],
            connection_field_secrets: vec![],
        };

        let mut tracker = HydrationTracker {
            hydrated: HashMap::new(),
        };

        persist_import_actions(actions, &mut tracker);

        assert!(
            tracker.hydrated.contains_key(&auth_id),
            "hydrate_auth_secret_fields must be called for the new auth profile"
        );
        let fields = &tracker.hydrated[&auth_id];
        assert!(
            fields.contains_key("token"),
            "the token field must be hydrated; got: {:?}",
            fields.keys().collect::<Vec<_>>()
        );
    }

    // -----------------------------------------------------------------
    // External client import
    // -----------------------------------------------------------------

    fn make_external_candidate(
        name: &str,
        secret: Option<&str>,
    ) -> datatab_portability::external::ExternalImportCandidate {
        datatab_portability::external::ExternalImportCandidate {
            name: name.to_string(),
            config: datatab_core::DbConfig::SQLite {
                path: std::path::PathBuf::from("/tmp/example.db"),
                connection_id: None,
            },
            kind: datatab_core::DbKind::SQLite,
            secret: secret.map(|s| SecretString::from(s.to_string())),
            secret_skip_reason: None,
        }
    }

    #[test]
    fn external_candidate_to_profile_sets_save_password_from_secret_presence() {
        let with_secret = make_external_candidate("Has Secret", Some("s3cret"));
        let profile = external_candidate_to_profile(&with_secret);
        assert_eq!(profile.name, "Has Secret");
        assert_eq!(profile.kind, Some(datatab_core::DbKind::SQLite));
        assert!(profile.save_password);

        let without_secret = make_external_candidate("No Secret", None);
        let profile = external_candidate_to_profile(&without_secret);
        assert!(!profile.save_password);
    }

    #[test]
    fn external_candidate_to_profile_generates_a_fresh_id_each_time() {
        let candidate = make_external_candidate("Demo", None);
        let first = external_candidate_to_profile(&candidate);
        let second = external_candidate_to_profile(&candidate);
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn persist_external_candidate_writes_secret_under_the_new_connection_ref() {
        let mut deps = FakePersistence::all_drivers();
        let mut outcome = ExternalPersistOutcome::default();
        let candidate = make_external_candidate("Demo", Some("s3cret"));

        persist_external_candidate(candidate, &mut deps, &mut outcome);

        assert_eq!(outcome.succeeded, vec!["Demo".to_string()]);
        assert!(outcome.secret_failures.is_empty());
        assert_eq!(deps.conn_names, vec!["Demo".to_string()]);
    }

    #[test]
    fn persist_external_candidate_records_secret_failure_without_dropping_the_connection() {
        let candidate = make_external_candidate("Demo", Some("s3cret"));
        let mut deps = FakePersistence::all_drivers().with_all_secrets_failing();
        let mut outcome = ExternalPersistOutcome::default();

        persist_external_candidate(candidate, &mut deps, &mut outcome);

        assert_eq!(
            deps.conn_names,
            vec!["Demo".to_string()],
            "a keyring failure must not undo the already-persisted connection"
        );
        assert_eq!(outcome.succeeded, vec!["Demo".to_string()]);
        assert_eq!(outcome.secret_failures, vec!["Demo".to_string()]);
    }

    #[test]
    fn persist_external_candidate_reports_needs_driver_for_unregistered_driver() {
        let mut deps = FakePersistence::with_drivers(&[]);
        let mut outcome = ExternalPersistOutcome::default();
        let candidate = make_external_candidate("Demo", None);

        persist_external_candidate(candidate, &mut deps, &mut outcome);

        assert!(outcome.succeeded.is_empty());
        assert_eq!(outcome.needs_driver.len(), 1);
        assert_eq!(outcome.needs_driver[0].0, "Demo");
    }

    #[test]
    fn default_candidate_includes_defaults_every_slot_to_true() {
        assert_eq!(default_candidate_includes(3), vec![true, true, true]);
        assert_eq!(default_candidate_includes(0), Vec::<bool>::new());
    }

    #[test]
    fn toggle_candidate_include_flips_only_the_targeted_index() {
        let mut includes = default_candidate_includes(3);
        toggle_candidate_include(&mut includes, 1);
        assert_eq!(includes, vec![true, false, true]);

        toggle_candidate_include(&mut includes, 1);
        assert_eq!(includes, vec![true, true, true]);
    }

    #[test]
    fn toggle_candidate_include_is_a_no_op_on_out_of_range_index() {
        let mut includes = default_candidate_includes(2);
        toggle_candidate_include(&mut includes, 5);
        assert_eq!(includes, vec![true, true]);
    }
}
