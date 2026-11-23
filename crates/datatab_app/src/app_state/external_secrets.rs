//! Secret-kind form fields of a profile. The extra secret fields of an RPC
//! (external) driver live in the OS keyring under `connection_field_secret_ref`;
//! the profile keeps only the field ids in `keyring_secret_fields`. They never
//! reach `external_values_json`, a value ref row as a literal, or a bundle. A
//! `Literal` value ref on any secret-kind field of any driver, the primary
//! `password` included, is settled here too and never written to the database.

use datatab_core::SecretStore;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use datatab_core::secrecy::{ExposeSecret, SecretString};
use datatab_core::{ConnectionProfile, DbConfig, DbDriver, ValueRef};

use super::{AppState, ProfileSaveError};

/// A secret field value that could not be written to the OS keyring. The
/// save or import is refused, naming the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFieldKeyringError {
    pub label: String,
    /// The fields whose saved value may have changed anyway: the keyring
    /// refused to put their old value back, or a write timed out and its
    /// abandoned worker may still finish. `None` when every entry is as it was.
    pub may_have_changed: Option<SecretFieldsMayHaveChanged>,
}

impl std::fmt::Display for SecretFieldKeyringError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "The secret field '{}' could not be saved to the system keyring. Unlock it and try again.",
            self.label
        )?;
        if let Some(may_have_changed) = &self.may_have_changed {
            write!(formatter, " {may_have_changed}")?;
        }
        Ok(())
    }
}

/// The secret fields of a refused save whose saved value may have changed,
/// by label, never with a value. The user checks them before connecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFieldsMayHaveChanged {
    pub labels: Vec<String>,
}

impl SecretFieldsMayHaveChanged {
    /// The labels, quoted and joined for a message.
    pub fn quoted_labels(&self) -> String {
        self.labels
            .iter()
            .map(|label| format!("'{label}'"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for SecretFieldsMayHaveChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "The saved value of these secret fields may have changed; check them before connecting: {}.",
            self.quoted_labels()
        )
    }
}

/// Why one keyring write or delete did not go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyringWriteFailure {
    /// The keyring answered no; the entry is as it was.
    Refused,
    /// The call timed out, or the store refused it while on hold after a
    /// timeout. An abandoned worker may still change the entry.
    TimedOut,
}

/// The secret-kind fields of an external profile's form other than the
/// primary `password`, as (id, label). Empty for a built-in config. `None`
/// when the driver is not registered, or the profile names no driver (its id
/// would fall back to a built-in driver's form), so the form, and which
/// values are secret, is unknown.
pub(crate) fn external_secret_fields(
    profile: &ConnectionProfile,
    drivers: &HashMap<String, Arc<dyn DbDriver>>,
) -> Option<Vec<(String, String)>> {
    if !matches!(profile.config, DbConfig::External { .. }) {
        return Some(Vec::new());
    }
    let driver = drivers.get(profile.driver_id.as_deref()?)?;
    Some(
        driver
            .form_definition()
            .secret_fields()
            .filter(|field| field.id != "password")
            .map(|field| (field.id.clone(), field.label.clone()))
            .collect(),
    )
}

/// Takes a field's typed value out of the profile: its config value, and a
/// `Literal` value ref, which is a typed value too. The ref wins when both
/// are set, as it does at connect.
fn take_typed_value(profile: &mut ConnectionProfile, field_id: &str) -> Option<String> {
    let config_value = match &mut profile.config {
        DbConfig::External { values, .. } => values.remove(field_id),
        _ => None,
    };
    let literal = match profile.value_refs.get(field_id) {
        Some(ValueRef::Literal { value }) => Some(value.clone()),
        _ => None,
    };
    if literal.is_some() {
        profile.value_refs.remove(field_id);
    }
    literal.or(config_value)
}

/// Removes the secret field values (config values and literal refs) of an
/// external profile whose form is known, without saving them anywhere. The
/// last guard before a profile is written. Returns how many were removed.
pub(crate) fn strip_external_secret_values(
    profile: &mut ConnectionProfile,
    drivers: &HashMap<String, Arc<dyn DbDriver>>,
) -> usize {
    let Some(fields) = external_secret_fields(profile, drivers) else {
        return 0;
    };
    fields
        .iter()
        .filter(|(id, _)| take_typed_value(profile, id).is_some())
        .count()
}

/// The ids of every secret-kind field of the profile's form, the primary
/// `password` included. `None` when the driver is not registered, or an
/// external profile names no driver, so which values are secret is unknown.
pub(crate) fn secret_field_ids(
    profile: &ConnectionProfile,
    drivers: &HashMap<String, Arc<dyn DbDriver>>,
) -> Option<Vec<String>> {
    let driver_id = if matches!(profile.config, DbConfig::External { .. }) {
        profile.driver_id.clone()?
    } else {
        profile.driver_id()
    };
    let driver = drivers.get(&driver_id)?;
    Some(
        driver
            .form_definition()
            .secret_fields()
            .map(|field| field.id.clone())
            .collect(),
    )
}

/// Removes every `Literal` value ref on a secret-kind field, for a driver of
/// any kind, without saving it anywhere. A literal is the secret itself, so
/// it never goes to the database. Returns how many were removed.
pub(crate) fn strip_literal_secret_refs(
    profile: &mut ConnectionProfile,
    drivers: &HashMap<String, Arc<dyn DbDriver>>,
) -> usize {
    let Some(fields) = secret_field_ids(profile, drivers) else {
        return 0;
    };
    let before = profile.value_refs.len();
    profile.value_refs.retain(|field_id, value_ref| {
        !(matches!(value_ref, ValueRef::Literal { .. }) && fields.contains(field_id))
    });
    before - profile.value_refs.len()
}

fn keyring_write_failure(error: &datatab_core::DbError) -> KeyringWriteFailure {
    if matches!(error, datatab_core::DbError::Timeout) {
        KeyringWriteFailure::TimedOut
    } else {
        KeyringWriteFailure::Refused
    }
}

/// Counts of secret values settled at load or save.
#[derive(Default)]
pub(super) struct LoadMigration {
    pub(super) moved: usize,
    pub(super) dropped: usize,
    /// Values and value refs under a field id the driver's form does not declare.
    pub(super) undeclared: usize,
}

impl LoadMigration {
    /// Logs how many values were moved or dropped, never a value.
    pub(super) fn log_counts(&self) {
        if self.undeclared > 0 {
            log::warn!(
                "Dropped {} stored value(s) or value ref(s) of external drivers under fields their forms \
                 no longer declare",
                self.undeclared
            );
        }
        if self.dropped > 0 {
            log::warn!(
                "Dropped {} secret value(s): the literal is empty, the slot already holds a value, \
                 no usable keyring slot is available, password saving is disabled, another value \
                 source is selected, or a non-password literal secret ref was stripped",
                self.dropped
            );
        }
        if self.moved > 0 {
            log::info!("Moved {} secret value(s) to the system keyring", self.moved);
        }
    }
}

const SELECTOR_FIELD_IDS: [&str; 4] = ["host", "database", "user", "password"];

/// Removes the stored values and value refs of an external profile whose
/// form is known under field ids the form does not declare (for example a
/// field a driver renamed or removed). Their kind is unknown, so they are
/// treated as secrets and dropped. The standard selectors remain valid even
/// when the form omits them. Returns how many were removed.
fn drop_undeclared_values(
    profile: &mut ConnectionProfile,
    drivers: &HashMap<String, Arc<dyn DbDriver>>,
    runtime: &datatab_storage::bootstrap::StorageRuntime,
) -> usize {
    if external_secret_fields(profile, drivers).is_none() {
        return 0;
    }
    let Some(driver) = profile
        .driver_id
        .as_deref()
        .and_then(|driver_id| drivers.get(driver_id))
    else {
        return 0;
    };
    let form = driver.form_definition();
    let DbConfig::External { values, .. } = &mut profile.config else {
        return 0;
    };
    let before = values.len() + profile.value_refs.len();
    values.retain(|field_id, _| {
        SELECTOR_FIELD_IDS.contains(&field_id.as_str()) || form.field(field_id).is_some()
    });
    profile.value_refs.retain(|field_id, _| {
        if SELECTOR_FIELD_IDS.contains(&field_id.as_str()) || form.field(field_id).is_some() {
            return true;
        }
        // A broken ref on another field makes saves preserve the stored ref
        // rows, so removing only the in-memory ref would leave this row behind.
        if let Err(error) = runtime
            .connection_profiles()
            .value_refs()
            .delete_for_profile_key(&profile.id.to_string(), field_id)
        {
            log::error!("Failed to delete an undeclared stored field reference: {error}");
        }
        false
    });
    before - values.len() - profile.value_refs.len()
}

/// One keyring change a save makes for a secret field: write a value, or
/// delete the entry when `value` is `None`.
struct PlannedChange {
    secret_ref: String,
    label: String,
    value: Option<SecretString>,
    /// Whether the profile marked the field before this save, so its entry
    /// may hold a value that a refused save must put back.
    was_marked: bool,
}

/// The keyring entries a save changed, each with what it held before. A
/// save refused after this point calls [`SecretFieldChanges::restore`], so
/// the profile's saved credentials stay as they were.
#[derive(Debug, Default)]
#[must_use = "a refused save must restore the keyring entries it changed"]
pub struct SecretFieldChanges {
    previous: Vec<AppliedChange>,
}

impl SecretFieldChanges {
    /// Whether the save changed no keyring entry, so a refused save has
    /// nothing to put back.
    pub fn is_empty(&self) -> bool {
        self.previous.is_empty()
    }
}

/// One keyring entry a save changed: its field label, and what it held
/// before (never logged).
#[derive(Debug)]
struct AppliedChange {
    secret_ref: String,
    label: String,
    previous: Option<SecretString>,
}

/// Takes the typed values out of the profile, updates its markers, and lists
/// the keyring changes the save needs. A marker of a field the form no longer
/// declares, or of a field that now has a value source, is dropped, and its
/// entry deleted.
fn plan_secret_field_changes(
    profile: &mut ConnectionProfile,
    fields: Vec<(String, String)>,
) -> Vec<PlannedChange> {
    let mut changes = Vec::new();
    let undeclared: Vec<String> = profile
        .keyring_secret_fields
        .iter()
        .filter(|marked| !fields.iter().any(|(id, _)| id == *marked))
        .cloned()
        .collect();
    for field_id in undeclared {
        profile.keyring_secret_fields.remove(&field_id);
        changes.push(PlannedChange {
            secret_ref: datatab_core::connection_field_secret_ref(&profile.id, &field_id),
            label: field_id,
            value: None,
            was_marked: true,
        });
    }

    for (field_id, label) in fields {
        let typed = take_typed_value(profile, &field_id);
        let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, &field_id);
        let was_marked = profile.keyring_secret_fields.contains(&field_id);

        if profile.value_refs.contains_key(&field_id) {
            if profile.keyring_secret_fields.remove(&field_id) {
                changes.push(PlannedChange {
                    secret_ref,
                    label,
                    value: None,
                    was_marked,
                });
            }
            continue;
        }

        let Some(value) = typed.filter(|value| !value.is_empty()) else {
            continue;
        };
        profile.keyring_secret_fields.insert(field_id);
        changes.push(PlannedChange {
            secret_ref,
            label,
            value: Some(SecretString::from(value)),
            was_marked,
        });
    }
    changes
}

impl AppState {
    /// Unknown forms cannot classify new literals. Check every profile before
    /// any write, and protect its stored rows with the same seam as broken refs.
    pub(super) fn value_ref_preservation_for_save(
        &self,
        unknown_form_ids: &std::collections::HashSet<uuid::Uuid>,
    ) -> Result<crate::config_loader::ProfileValueRefDiagnostics, ProfileSaveError> {
        let mut preservation = self.value_ref_diagnostics.clone();
        let mut refused = 0;
        for profile in &self.facade.profiles.profiles {
            if !unknown_form_ids.contains(&profile.id) {
                continue;
            }
            if crate::config_loader::has_new_literal_value_refs(&self.storage_runtime, profile)? {
                refused += 1;
            }
            preservation.entry(profile.id).or_default();
        }
        if refused > 0 {
            return Err(ProfileSaveError::UnknownFormLiteral { profiles: refused });
        }
        Ok(preservation)
    }

    /// Plans a connection-manager save without touching the keyring. Apply the
    /// returned step on the background executor before writing the profile row.
    pub fn prepare_external_secret_fields(
        &self,
        profile: &mut ConnectionProfile,
    ) -> SecretFieldWrite {
        Self::prepare_external_secret_fields_with(
            profile,
            &self.facade.connections.drivers,
            self.secret_store(),
        )
    }

    pub(crate) fn prepare_external_secret_fields_with(
        profile: &mut ConnectionProfile,
        drivers: &HashMap<String, Arc<dyn DbDriver>>,
        store: Arc<RwLock<Box<dyn SecretStore>>>,
    ) -> SecretFieldWrite {
        let planned = external_secret_fields(profile, drivers)
            .map(|fields| plan_secret_field_changes(profile, fields))
            .unwrap_or_default();
        SecretFieldWrite { store, planned }
    }
}

/// Plain keyring work, independent of application and UI state.
pub struct SecretFieldWrite {
    store: Arc<RwLock<Box<dyn SecretStore>>>,
    planned: Vec<PlannedChange>,
}

impl SecretFieldWrite {
    /// Reads rollback values and applies the planned changes in order.
    pub fn apply(self) -> Result<SecretFieldChanges, SecretFieldKeyringError> {
        let Self { store, planned } = self;
        // Every old value is read before anything is written, so a keyring
        // that cannot be read refuses the save with nothing changed.
        let mut previous_values = Vec::with_capacity(planned.len());
        for change in &planned {
            let previous = if change.was_marked {
                read_secret_entry(&store, &change.secret_ref).map_err(|()| {
                    SecretFieldKeyringError {
                        label: change.label.clone(),
                        may_have_changed: None,
                    }
                })?
            } else {
                None
            };
            previous_values.push(previous);
        }

        let mut applied = SecretFieldChanges::default();
        for (change, previous) in planned.into_iter().zip(previous_values) {
            let done = match &change.value {
                Some(value) => write_secret_entry(&store, &change.secret_ref, value),
                None => delete_secret_entry(&store, &change.secret_ref),
            };
            if let Err(failure) = done {
                let mut labels = match applied.restore(&store) {
                    Ok(()) => Vec::new(),
                    Err(unrestored) => unrestored.labels,
                };
                if failure == KeyringWriteFailure::TimedOut && !labels.contains(&change.label) {
                    labels.push(change.label.clone());
                }
                return Err(SecretFieldKeyringError {
                    label: change.label,
                    may_have_changed: (!labels.is_empty())
                        .then_some(SecretFieldsMayHaveChanged { labels }),
                });
            }
            applied.previous.push(AppliedChange {
                secret_ref: change.secret_ref,
                label: change.label,
                previous,
            });
        }
        Ok(applied)
    }
}

impl SecretFieldChanges {
    /// Puts back the keyring entries a refused save changed, newest first.
    ///
    /// A keyring may refuse to put a value back, for example while it is on
    /// hold after a timed-out write. The save is still refused; the returned
    /// error names each field whose saved value may have changed, so the user
    /// can check it before connecting.
    pub fn restore(
        self,
        store: &RwLock<Box<dyn SecretStore>>,
    ) -> Result<(), SecretFieldsMayHaveChanged> {
        let mut labels = Vec::new();
        for change in self.previous.into_iter().rev() {
            let restored = match &change.previous {
                Some(value) => write_secret_entry(store, &change.secret_ref, value),
                None => delete_secret_entry(store, &change.secret_ref),
            };
            if restored.is_err() && !labels.contains(&change.label) {
                labels.push(change.label);
            }
        }
        if labels.is_empty() {
            return Ok(());
        }
        log::error!(
            "Could not restore {} keyring entry(ies) of a refused save",
            labels.len()
        );
        labels.reverse();
        Err(SecretFieldsMayHaveChanged { labels })
    }
}

/// Writes one keyring entry, telling a timeout from a refusal.
fn write_secret_entry(
    store: &RwLock<Box<dyn SecretStore>>,
    secret_ref: &str,
    value: &SecretString,
) -> Result<(), KeyringWriteFailure> {
    let store = match store.read() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    if !store.is_available() {
        return Err(KeyringWriteFailure::Refused);
    }
    store.set(secret_ref, value).map_err(|error| {
        log::warn!("Failed to write a secret field to the keyring: {error}");
        keyring_write_failure(&error)
    })
}

/// Reads one keyring entry, telling a missing entry (`Ok(None)`) from a
/// keyring that cannot be read (`Err`).
fn read_secret_entry(
    store: &RwLock<Box<dyn SecretStore>>,
    secret_ref: &str,
) -> Result<Option<SecretString>, ()> {
    let store = match store.read() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    if !store.is_available() {
        return Err(());
    }
    store.get(secret_ref).map_err(|error| {
        log::warn!("Failed to read a secret field from the keyring: {error}");
    })
}

/// Deletes one keyring entry, telling a timeout from a refusal.
fn delete_secret_entry(
    store: &RwLock<Box<dyn SecretStore>>,
    secret_ref: &str,
) -> Result<(), KeyringWriteFailure> {
    let store = match store.read() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    if !store.is_available() {
        return Err(KeyringWriteFailure::Refused);
    }
    store.delete(secret_ref).map_err(|error| {
        log::warn!("Failed to delete a secret field from the keyring: {error}");
        keyring_write_failure(&error)
    })
}

impl AppState {
    /// The secret fields an imported profile keeps in their own keyring
    /// entries: the form's secret-kind fields other than `password`, when the
    /// driver is registered and builds an external config from the profile's
    /// values. Empty otherwise, so a built-in driver keeps its secret in the
    /// connection password slot.
    pub(crate) fn imported_keyring_field_ids_with(
        profile: &ConnectionProfile,
        drivers: &HashMap<String, Arc<dyn DbDriver>>,
    ) -> Vec<String> {
        let DbConfig::External { values, .. } = &profile.config else {
            return Vec::new();
        };
        let Some(driver) = drivers.get(&profile.driver_id()) else {
            return Vec::new();
        };
        if !matches!(driver.build_config(values), Ok(DbConfig::External { .. })) {
            return Vec::new();
        }
        driver
            .form_definition()
            .secret_fields()
            .filter(|field| field.id != "password")
            .map(|field| field.id.clone())
            .collect()
    }

    /// Every secret-kind field of the profile's registered driver form, the
    /// primary `password` included. Empty when the driver is not registered.
    pub(crate) fn secret_field_ids_with(
        profile: &ConnectionProfile,
        drivers: &HashMap<String, Arc<dyn DbDriver>>,
    ) -> Vec<String> {
        secret_field_ids(profile, drivers).unwrap_or_default()
    }

    /// Deletes the keyring entries of a profile's secret fields.
    pub(super) fn delete_external_secret_fields(&self, profile: &ConnectionProfile) {
        for field_id in &profile.keyring_secret_fields {
            let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, field_id);
            self.facade.secrets.delete_by_ref(&secret_ref);
        }
    }

    /// Prepares the copy of a profile for a duplicate. The copy does not
    /// share the source's keyring entries, and reading them here would read
    /// the keyring on the UI thread, so its secret fields start empty. When
    /// the driver is not registered, which values are secret is unknown, so
    /// the copy gets none of the external values.
    pub fn prepare_duplicate_external_values(&self, copy: &mut ConnectionProfile) {
        copy.keyring_secret_fields.clear();
        if external_secret_fields(copy, &self.facade.connections.drivers).is_some() {
            return;
        }
        if let DbConfig::External { values, .. } = &mut copy.config {
            values.clear();
        }
        copy.value_refs
            .retain(|_, value_ref| !matches!(value_ref, ValueRef::Literal { .. }));
    }

    /// Rows written before secret fields were kept out of the database may
    /// still hold their values. Now that the forms are known, each value is
    /// moved to the keyring, or dropped when the keyring cannot take it, and
    /// the rows are written again without them. A profile whose driver is
    /// not registered is left as it is until its form is known.
    pub(super) fn migrate_external_secret_values(&mut self) {
        let mut migration = LoadMigration::default();
        let drivers = self.facade.connections.drivers.clone();
        let mut profiles = std::mem::take(&mut self.facade.profiles.profiles);
        for profile in &mut profiles {
            self.migrate_profile_secret_values(profile, &drivers, &mut migration);
        }
        self.facade.profiles.profiles = profiles;

        if migration.moved + migration.dropped + migration.undeclared == 0 {
            return;
        }
        migration.log_counts();
        self.save_profiles();
    }

    fn migrate_profile_secret_values(
        &self,
        profile: &mut ConnectionProfile,
        drivers: &HashMap<String, Arc<dyn DbDriver>>,
        migration: &mut LoadMigration,
    ) {
        self.settle_literal_secret_refs(profile, drivers, migration);
        let Some(fields) = external_secret_fields(profile, drivers) else {
            return;
        };
        migration.undeclared += drop_undeclared_values(profile, drivers, &self.storage_runtime);
        for (field_id, _) in fields {
            let had_literal_ref = matches!(
                profile.value_refs.get(&field_id),
                Some(ValueRef::Literal { .. })
            );
            let Some(value) = take_typed_value(profile, &field_id) else {
                continue;
            };
            // A profile with a broken value ref keeps its stored ref rows on
            // save, so the literal row is deleted here.
            if had_literal_ref
                && let Err(error) = self
                    .storage_runtime
                    .connection_profiles()
                    .value_refs()
                    .delete_for_profile_key(&profile.id.to_string(), &field_id)
            {
                log::error!("Failed to delete a stored secret field reference: {error}");
            }
            if value.is_empty() {
                continue;
            }
            let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, &field_id);
            if !profile.value_refs.contains_key(&field_id)
                && self
                    .facade
                    .secrets
                    .set_by_ref(&secret_ref, &SecretString::from(value))
            {
                profile.keyring_secret_fields.insert(field_id);
                migration.moved += 1;
            } else {
                migration.dropped += 1;
            }
        }
    }

    /// A `Literal` value ref on a secret-kind field is the secret itself, so
    /// it is taken out of the profile and its stored row is deleted. A
    /// literal password goes to the password slot only when the profile saves
    /// its password and the slot is empty: a slot that holds a value, or one
    /// that cannot be read or written, keeps what it has and the literal is
    /// dropped. A literal on any other secret field of a built-in driver is
    /// dropped. Only `Literal` refs are touched; the extra secret fields of an
    /// external driver are handled by the caller. Nothing is logged but counts.
    pub(super) fn settle_literal_secret_refs(
        &self,
        profile: &mut ConnectionProfile,
        drivers: &HashMap<String, Arc<dyn DbDriver>>,
        migration: &mut LoadMigration,
    ) {
        let Some(fields) = secret_field_ids(profile, drivers) else {
            return;
        };
        let external = matches!(profile.config, DbConfig::External { .. });
        for field_id in fields {
            if external && field_id != "password" {
                continue;
            }
            if !matches!(
                profile.value_refs.get(&field_id),
                Some(ValueRef::Literal { .. })
            ) {
                continue;
            }
            let Some(ValueRef::Literal { value }) = profile.value_refs.remove(&field_id) else {
                continue;
            };
            if let Err(error) = self
                .storage_runtime
                .connection_profiles()
                .value_refs()
                .delete_for_profile_key(&profile.id.to_string(), &field_id)
            {
                log::error!("Failed to delete a stored secret field reference: {error}");
            }
            if field_id == "password" && self.store_literal_password(profile, value) {
                migration.moved += 1;
            } else {
                migration.dropped += 1;
            }
        }
    }

    /// Writes a literal password to the profile's password slot when the
    /// profile saves its password and the slot is empty.
    fn store_literal_password(&self, profile: &ConnectionProfile, value: String) -> bool {
        if !profile.save_password || value.is_empty() {
            return false;
        }
        let slot_is_empty = match self.get_password(profile) {
            Ok(None) => true,
            Ok(Some(existing)) => existing.expose_secret().is_empty(),
            Err(_) => false,
        };
        slot_is_empty
            && self
                .save_password(profile, &SecretString::from(value))
                .is_ok()
    }
}
