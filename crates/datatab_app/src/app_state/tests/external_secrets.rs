use super::*;
use datatab_core::ValueRef;
use std::sync::Mutex;

const SENTINEL: &str = "sentinel-api-token-7f3a91";

static FORM: std::sync::LazyLock<datatab_core::DriverFormDef> = std::sync::LazyLock::new(|| {
    use datatab_core::{FormFieldKind, FormSection, FormTab, field};
    datatab_core::DriverFormDef {
        tabs: vec![FormTab {
            id: "main".into(),
            label: "Main".into(),
            sections: vec![FormSection {
                title: "Auth".into(),
                icon: None,
                fields: vec![
                    field("host", "Host", FormFieldKind::Text, ""),
                    field("api_token", "API token", FormFieldKind::Password, ""),
                    field(
                        "client_secret",
                        "Client secret",
                        FormFieldKind::WriteOnly,
                        "",
                    ),
                    field("password", "Password", FormFieldKind::Password, ""),
                ],
            }],
        }],
    }
});

struct FakeRpcDriver {
    metadata: datatab_core::DriverMetadata,
    form: datatab_core::DriverFormDef,
}

impl DbDriver for FakeRpcDriver {
    fn kind(&self) -> datatab_core::DbKind {
        datatab_core::DbKind::Postgres
    }
    fn metadata(&self) -> &datatab_core::DriverMetadata {
        &self.metadata
    }
    fn form_definition(&self) -> &datatab_core::DriverFormDef {
        &self.form
    }
    fn driver_key(&self) -> datatab_core::DriverKey {
        "rpc:fake".into()
    }
    fn build_config(&self, values: &datatab_core::FormValues) -> Result<DbConfig, DbError> {
        Ok(DbConfig::External {
            kind: datatab_core::DbKind::SQLite,
            values: values.clone(),
        })
    }
    fn extract_values(&self, config: &DbConfig) -> datatab_core::FormValues {
        match config {
            DbConfig::External { values, .. } => values.clone(),
            _ => datatab_core::FormValues::new(),
        }
    }
    fn connect_with_secrets(
        &self,
        _: &ConnectionProfile,
        _: Option<&SecretString>,
        _: Option<&SecretString>,
    ) -> Result<Box<dyn datatab_core::Connection>, DbError> {
        Err(DbError::NotSupported("test driver".into()))
    }
    fn test_connection(&self, _: &ConnectionProfile) -> Result<(), DbError> {
        Ok(())
    }
}

/// An in-memory keyring. `locked` makes every write fail, as a locked
/// keyring does; `fail_field` makes the writes of one field fail.
#[derive(Clone, Default)]
struct MemoryKeyring {
    values: Arc<Mutex<HashMap<String, String>>>,
    locked: bool,
    fail_field: Option<&'static str>,
}

impl MemoryKeyring {
    fn get(&self, secret_ref: &str) -> Option<String> {
        self.values
            .lock()
            .expect("keyring lock")
            .get(secret_ref)
            .cloned()
    }
}

impl SecretStore for MemoryKeyring {
    fn is_available(&self) -> bool {
        true
    }

    fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, DbError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| DbError::connection_failed("keyring lock poisoned"))?
            .get(secret_ref)
            .cloned()
            .map(SecretString::from))
    }

    fn set(&self, secret_ref: &str, value: &SecretString) -> Result<(), DbError> {
        let failing_field = self
            .fail_field
            .is_some_and(|field| secret_ref.ends_with(&format!(":field:{field}")));
        if self.locked || failing_field {
            return Err(DbError::IoError(std::io::Error::other("keyring locked")));
        }
        self.values
            .lock()
            .map_err(|_| DbError::connection_failed("keyring lock poisoned"))?
            .insert(
                secret_ref.to_string(),
                datatab_core::secrecy::ExposeSecret::expose_secret(value).to_string(),
            );
        Ok(())
    }

    fn delete(&self, secret_ref: &str) -> Result<(), DbError> {
        self.values
            .lock()
            .map_err(|_| DbError::connection_failed("keyring lock poisoned"))?
            .remove(secret_ref);
        Ok(())
    }
}

fn drivers() -> HashMap<String, Arc<dyn DbDriver>> {
    drivers_with_form(FORM.clone())
}

fn drivers_with_form(form: datatab_core::DriverFormDef) -> HashMap<String, Arc<dyn DbDriver>> {
    let metadata = datatab_core::DriverMetadataBuilder::new(
        "fake",
        "Fake",
        datatab_core::DatabaseCategory::Relational,
        datatab_core::QueryLanguage::Sql,
    )
    .build();
    HashMap::from([(
        "rpc:fake".to_string(),
        Arc::new(FakeRpcDriver { metadata, form }) as Arc<dyn DbDriver>,
    )])
}

fn profile_with_token(token: &str) -> ConnectionProfile {
    let values = HashMap::from([
        ("host".to_string(), "db.example".to_string()),
        ("api_token".to_string(), token.to_string()),
    ]);
    ConnectionProfile::new_with_driver(
        "external",
        datatab_core::DbKind::SQLite,
        "rpc:fake",
        DbConfig::External {
            kind: datatab_core::DbKind::SQLite,
            values,
        },
    )
}

fn external_values(profile: &ConnectionProfile) -> &HashMap<String, String> {
    let DbConfig::External { values, .. } = &profile.config else {
        panic!("not an external config");
    };
    values
}

fn state_with_keyring(
    drivers: HashMap<String, Arc<dyn DbDriver>>,
    keyring: &MemoryKeyring,
) -> AppState {
    let mut state = test_state_with_profiles(drivers, Vec::new());
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(keyring.clone()));
    state
}

fn database_bytes(runtime: &datatab_storage::bootstrap::StorageRuntime) -> Vec<u8> {
    let path = runtime.datatab_db_path().to_path_buf();
    let mut bytes = std::fs::read(&path).unwrap_or_default();
    let mut wal = path.into_os_string();
    wal.push("-wal");
    bytes.extend(std::fs::read(wal).unwrap_or_default());
    bytes
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Stores the secret fields of a save that goes through.
fn store(state: &AppState, profile: &mut ConnectionProfile) {
    let _stored = state
        .prepare_external_secret_fields(profile)
        .apply()
        .expect("the keyring takes the values");
}

fn field_ref(profile: &ConnectionProfile) -> String {
    datatab_core::connection_field_secret_ref(&profile.id, "api_token")
}

#[test]
fn a_typed_secret_field_goes_to_the_keyring_and_not_the_database() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token(SENTINEL);

    store(&state, &mut profile);
    state.add_profile_in_folder(profile.clone(), None);

    assert_eq!(keyring.get(&field_ref(&profile)).as_deref(), Some(SENTINEL));
    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");
    assert!(!external_values(&loaded.profiles[0]).contains_key("api_token"));
    assert!(
        loaded.profiles[0]
            .keyring_secret_fields
            .contains("api_token")
    );
    let bytes = database_bytes(&state.storage_runtime);
    assert!(contains(&bytes, "db.example"), "the database file was read");
    assert!(!contains(&bytes, SENTINEL));
}

#[test]
fn a_literal_value_ref_on_a_secret_field_goes_to_the_keyring() {
    let keyring = MemoryKeyring::default();
    let state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("");
    profile.value_refs.insert(
        "api_token".into(),
        datatab_core::ValueRef::literal(SENTINEL),
    );

    store(&state, &mut profile);

    assert!(profile.value_refs.is_empty());
    assert_eq!(keyring.get(&field_ref(&profile)).as_deref(), Some(SENTINEL));
}

#[test]
fn a_later_save_with_the_field_left_empty_keeps_the_value_out_of_the_row() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token(SENTINEL);
    store(&state, &mut profile);
    state.add_profile_in_folder(profile.clone(), None);

    // The edit form leaves the field empty and carries the saved marker.
    let mut edited = profile_with_token("");
    edited.id = profile.id;
    edited.name = "renamed".into();
    edited.keyring_secret_fields = profile.keyring_secret_fields.clone();
    store(&state, &mut edited);
    state.update_profile(edited);

    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");
    assert_eq!(loaded.profiles[0].name, "renamed");
    assert!(!external_values(&loaded.profiles[0]).contains_key("api_token"));
    assert!(
        loaded.profiles[0]
            .keyring_secret_fields
            .contains("api_token")
    );
    assert_eq!(keyring.get(&field_ref(&profile)).as_deref(), Some(SENTINEL));
    assert!(!contains(&database_bytes(&state.storage_runtime), SENTINEL));
}

#[test]
fn a_keyring_that_refuses_the_value_refuses_the_save() {
    let keyring = MemoryKeyring {
        locked: true,
        ..MemoryKeyring::default()
    };
    let state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token(SENTINEL);

    let error = state
        .prepare_external_secret_fields(&mut profile)
        .apply()
        .expect_err("a locked keyring refuses the save");

    assert_eq!(error.label, "API token");
    assert!(!error.to_string().contains(SENTINEL));
}

#[test]
fn an_unavailable_keyring_refuses_the_save() {
    let mut state = test_state_with_profiles(drivers(), Vec::new());
    state.facade.secrets =
        datatab_core::SecretManager::new(Box::new(datatab_core::NoopSecretStore));
    let mut profile = profile_with_token(SENTINEL);

    let error = state
        .prepare_external_secret_fields(&mut profile)
        .apply()
        .expect_err("no keyring refuses the save");

    assert_eq!(error.label, "API token");
}

#[test]
fn deleting_the_profile_deletes_its_secret_fields() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token(SENTINEL);
    store(&state, &mut profile);
    state.add_profile_in_folder(profile.clone(), None);

    let index = state
        .profiles()
        .iter()
        .position(|item| item.id == profile.id)
        .expect("saved profile");
    state.remove_profile(index);

    assert_eq!(keyring.get(&field_ref(&profile)), None);
}

#[test]
fn an_old_row_moves_its_secret_value_to_the_keyring() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let old = profile_with_token(SENTINEL);
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");
    assert!(contains(&database_bytes(&state.storage_runtime), SENTINEL));

    state.facade.profiles.profiles = crate::config_loader::load_config(&state.storage_runtime)
        .expect("load")
        .profiles;
    state.migrate_external_secret_values();

    let migrated = &state.profiles()[0];
    assert!(!external_values(migrated).contains_key("api_token"));
    assert!(migrated.keyring_secret_fields.contains("api_token"));
    assert_eq!(keyring.get(&field_ref(&old)).as_deref(), Some(SENTINEL));
    let rewritten = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(!external_values(&rewritten.profiles[0]).contains_key("api_token"));
    assert!(
        rewritten.profiles[0]
            .keyring_secret_fields
            .contains("api_token")
    );
}

/// A literal value ref is a typed value too. The profile also has a broken
/// ref, so a save keeps its stored ref rows: the literal row must still go.
#[test]
fn an_old_literal_ref_moves_to_the_keyring_beside_a_broken_ref() {
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;

    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut old = profile_with_token("");
    old.value_refs.insert(
        "api_token".into(),
        datatab_core::ValueRef::literal(SENTINEL),
    );
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");
    let mut broken = ConnectionProfileValueRefDto::new_literal(
        old.id.to_string(),
        "host".into(),
        "unused".into(),
    );
    broken.ref_kind = "retired-kind".into();
    state
        .storage_runtime
        .connection_profiles()
        .value_refs()
        .insert(&broken)
        .expect("store a broken ref");

    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");
    assert!(loaded.value_ref_diagnostics.contains_key(&old.id));
    state.facade.profiles.profiles = loaded.profiles;
    state.value_ref_diagnostics = loaded.value_ref_diagnostics;
    state.migrate_external_secret_values();

    assert_eq!(keyring.get(&field_ref(&old)).as_deref(), Some(SENTINEL));
    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(!reloaded.profiles[0].value_refs.contains_key("api_token"));
    assert!(
        reloaded.profiles[0]
            .keyring_secret_fields
            .contains("api_token")
    );
}

#[test]
fn an_old_row_drops_its_secret_value_when_the_keyring_refuses_it() {
    let keyring = MemoryKeyring {
        locked: true,
        ..MemoryKeyring::default()
    };
    let mut state = state_with_keyring(drivers(), &keyring);
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        &[profile_with_token(SENTINEL)],
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");

    state.facade.profiles.profiles = crate::config_loader::load_config(&state.storage_runtime)
        .expect("load")
        .profiles;
    state.migrate_external_secret_values();

    let rewritten = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(!external_values(&rewritten.profiles[0]).contains_key("api_token"));
    assert!(rewritten.profiles[0].keyring_secret_fields.is_empty());
}

#[test]
fn an_old_row_of_an_unregistered_driver_is_left_as_it_is() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(HashMap::new(), &keyring);
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        &[profile_with_token(SENTINEL)],
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");
    state.facade.profiles.profiles = crate::config_loader::load_config(&state.storage_runtime)
        .expect("load")
        .profiles;

    state.migrate_external_secret_values();

    assert_eq!(
        external_values(&state.profiles()[0])
            .get("api_token")
            .map(String::as_str),
        Some(SENTINEL)
    );
    assert!(keyring.values.lock().expect("keyring lock").is_empty());
}

#[test]
fn a_duplicate_starts_without_the_secret_fields_and_reads_no_keyring() {
    let mut state = test_state_with_profiles(drivers(), Vec::new());
    state.facade.secrets =
        datatab_core::SecretManager::new(Box::new(super::refusal::PanicOnReadSecretStore));
    let mut copy = profile_with_token("");
    copy.keyring_secret_fields.insert("api_token".into());

    state.prepare_duplicate_external_values(&mut copy);

    assert!(copy.keyring_secret_fields.is_empty());
    assert_eq!(
        external_values(&copy).get("host").map(String::as_str),
        Some("db.example"),
        "values that are not secret are copied"
    );
}

#[test]
fn duplicating_a_profile_of_an_unregistered_driver_copies_no_value() {
    let keyring = MemoryKeyring::default();
    let state = state_with_keyring(HashMap::new(), &keyring);
    let mut copy = profile_with_token(SENTINEL);
    copy.value_refs.insert(
        "secret_key".into(),
        datatab_core::ValueRef::literal(SENTINEL),
    );

    state.prepare_duplicate_external_values(&mut copy);

    assert!(external_values(&copy).is_empty());
    assert!(copy.value_refs.is_empty());
    assert!(!format!("{copy:?}").contains(SENTINEL));
}

#[test]
fn export_of_an_unregistered_driver_carries_no_value() {
    use datatab_portability::{
        ConnectionWithValues, EncryptionChoice, ExportGraph, ExportOptions, IncludeExclude,
    };

    let profile = profile_with_token(SENTINEL);
    for connection_password in [IncludeExclude::Include, IncludeExclude::Exclude] {
        let graph = ExportGraph {
            connections: vec![ConnectionWithValues {
                profile: &profile,
                values: external_values(&profile).clone(),
            }],
            auth_profiles: Vec::new(),
            ssh_tunnels: Vec::new(),
            proxies: Vec::new(),
        };
        let opts = ExportOptions {
            include_hooks: false,
            include_settings_overrides: false,
            embed_ssh_keys: false,
            encryption: EncryptionChoice::Plaintext { forced: true },
            connection_password,
            proxy_credentials: IncludeExclude::Exclude,
            ssh_password: IncludeExclude::Exclude,
            auth_modes: Default::default(),
            per_secret_overrides: Default::default(),
        };

        let (bytes, _) = datatab_portability::export::export(
            &graph,
            &opts,
            &crate::portability::AppFieldHintResolver::new(HashMap::new()),
            &crate::portability::AppExportTransformResolver::new(HashMap::new()),
            &NoSecrets,
        )
        .expect("export");

        assert!(contains(&bytes, "external"), "the connection was exported");
        assert!(!contains(&bytes, SENTINEL));
        assert!(!contains(&bytes, "db.example"));
    }
}

struct NoSecrets;

impl datatab_portability::SecretReader for NoSecrets {
    fn read(&self, _: &str) -> Option<SecretString> {
        None
    }
}

#[test]
fn export_hint_for_a_secret_field_comes_from_the_form_kind() {
    use datatab_core::ExportFieldHint;
    use datatab_portability::FieldHintResolver;
    let resolver = crate::portability::AppFieldHintResolver::new(drivers());
    let profile = profile_with_token(SENTINEL);
    let values = external_values(&profile).clone();

    assert_eq!(
        resolver.hint(&profile, "api_token", &values),
        ExportFieldHint::Secret
    );
    assert_eq!(
        resolver.hint(&profile, "host", &values),
        ExportFieldHint::Include
    );
}

fn profile_with_two_secrets(api_token: &str, client_secret: &str) -> ConnectionProfile {
    let mut profile = profile_with_token(api_token);
    if let DbConfig::External { values, .. } = &mut profile.config {
        values.insert("client_secret".into(), client_secret.into());
    }
    profile
}

fn client_secret_ref(profile: &ConnectionProfile) -> String {
    datatab_core::connection_field_secret_ref(&profile.id, "client_secret")
}

/// An edit whose second secret field cannot be written leaves the first one
/// as it was, not with the new value (#411 triage round 2, point 1).
#[test]
fn a_refused_edit_leaves_every_saved_secret_field_as_it_was() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_two_secrets("old-token", "old-secret");
    store(&state, &mut profile);
    state.add_profile_in_folder(profile.clone(), None);

    let failing = MemoryKeyring {
        fail_field: Some("client_secret"),
        ..keyring.clone()
    };
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(failing));
    let mut edited = profile_with_two_secrets("new-token", "new-secret");
    edited.id = profile.id;
    edited.keyring_secret_fields = profile.keyring_secret_fields.clone();

    let error = state
        .prepare_external_secret_fields(&mut edited)
        .apply()
        .expect_err("the second field cannot be written");

    assert_eq!(error.label, "Client secret");
    assert_eq!(
        keyring.get(&field_ref(&profile)).as_deref(),
        Some("old-token")
    );
    assert_eq!(
        keyring.get(&client_secret_ref(&profile)).as_deref(),
        Some("old-secret")
    );
}

/// A new profile whose second secret field cannot be written leaves no entry
/// for the first one.
#[test]
fn a_refused_new_profile_leaves_no_keyring_entry() {
    let keyring = MemoryKeyring {
        fail_field: Some("client_secret"),
        ..MemoryKeyring::default()
    };
    let state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_two_secrets(SENTINEL, "secret");

    state
        .prepare_external_secret_fields(&mut profile)
        .apply()
        .expect_err("the second field cannot be written");

    assert_eq!(keyring.get(&field_ref(&profile)), None);
}

/// A save refused after the secret fields were stored (for example by the
/// primary password write) puts the old entries back and removes new ones.
#[test]
fn restoring_the_changes_of_a_refused_save_puts_the_old_entries_back() {
    let keyring = MemoryKeyring::default();
    let state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("old-token");
    store(&state, &mut profile);

    let mut edited = profile_with_two_secrets("new-token", "new-secret");
    edited.id = profile.id;
    edited.keyring_secret_fields = profile.keyring_secret_fields.clone();
    let changes = state
        .prepare_external_secret_fields(&mut edited)
        .apply()
        .expect("the keyring takes the values");
    assert_eq!(
        keyring.get(&field_ref(&profile)).as_deref(),
        Some("new-token")
    );

    changes
        .restore(&state.secret_store())
        .expect("the keyring takes the old values back");

    assert_eq!(
        keyring.get(&field_ref(&profile)).as_deref(),
        Some("old-token")
    );
    assert_eq!(keyring.get(&client_secret_ref(&profile)), None);
}

/// A marker of a field the form no longer declares is dropped with its
/// keyring entry, so no entry is left behind.
#[test]
fn a_dropped_marker_deletes_its_keyring_entry() {
    let keyring = MemoryKeyring::default();
    let state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("");
    let removed_ref = datatab_core::connection_field_secret_ref(&profile.id, "removed_field");
    keyring
        .values
        .lock()
        .expect("keyring lock")
        .insert(removed_ref.clone(), "old".into());
    profile.keyring_secret_fields.insert("removed_field".into());

    store(&state, &mut profile);

    assert!(profile.keyring_secret_fields.is_empty());
    assert_eq!(keyring.get(&removed_ref), None);
}

/// An external profile with no driver id would fall back to a built-in
/// driver's form, which says nothing about its values. It is treated as a
/// profile whose form is unknown: a duplicate copies none of its values
/// (#411 triage round 2, point 4).
#[test]
fn a_profile_without_a_driver_id_is_treated_as_an_unknown_form() {
    let builtin_id = ConnectionProfile::builtin_driver_id_for_kind(datatab_core::DbKind::SQLite);
    let fake = drivers()
        .remove("rpc:fake")
        .expect("the fake driver is registered");
    let state = state_with_keyring(
        HashMap::from([(builtin_id.to_string(), fake)]),
        &MemoryKeyring::default(),
    );
    let mut copy = profile_with_token(SENTINEL);
    copy.driver_id = None;

    state.prepare_duplicate_external_values(&mut copy);

    assert!(external_values(&copy).is_empty());
}

/// A stored value under a field id the driver's form does not declare has an
/// unknown kind, so it is dropped at load and the row is rewritten without
/// it; declared values stay (#411 triage round 2, point 5).
#[test]
fn an_old_row_drops_a_value_its_form_does_not_declare() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut old = profile_with_token("");
    if let DbConfig::External { values, .. } = &mut old.config {
        values.insert("renamed_token".into(), SENTINEL.into());
    }
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");
    assert!(contains(&database_bytes(&state.storage_runtime), SENTINEL));

    state.facade.profiles.profiles = crate::config_loader::load_config(&state.storage_runtime)
        .expect("load")
        .profiles;
    state.migrate_external_secret_values();

    let migrated = &state.profiles()[0];
    assert!(!external_values(migrated).contains_key("renamed_token"));
    assert_eq!(
        external_values(migrated).get("host").map(String::as_str),
        Some("db.example")
    );
    let rewritten = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(!external_values(&rewritten.profiles[0]).contains_key("renamed_token"));
    assert!(keyring.values.lock().expect("keyring lock").is_empty());
}

/// A keyring whose write of `client_secret` times out. As the real store does
/// after a timeout, it then refuses every write until its cooldown ends.
#[derive(Clone, Default)]
struct CooldownKeyring {
    values: Arc<Mutex<HashMap<String, String>>>,
    on_hold: Arc<Mutex<bool>>,
}

impl CooldownKeyring {
    fn hold(&self) -> std::sync::MutexGuard<'_, bool> {
        match self.on_hold.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl SecretStore for CooldownKeyring {
    fn is_available(&self) -> bool {
        true
    }

    fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, DbError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| DbError::connection_failed("keyring lock poisoned"))?
            .get(secret_ref)
            .cloned()
            .map(SecretString::from))
    }

    fn set(&self, secret_ref: &str, value: &SecretString) -> Result<(), DbError> {
        let mut on_hold = self.hold();
        if *on_hold {
            return Err(DbError::Timeout);
        }
        if secret_ref.ends_with(":field:client_secret") {
            *on_hold = true;
            return Err(DbError::Timeout);
        }
        self.values
            .lock()
            .map_err(|_| DbError::connection_failed("keyring lock poisoned"))?
            .insert(
                secret_ref.to_string(),
                datatab_core::secrecy::ExposeSecret::expose_secret(value).to_string(),
            );
        Ok(())
    }

    fn delete(&self, secret_ref: &str) -> Result<(), DbError> {
        if *self.hold() {
            return Err(DbError::Timeout);
        }
        self.values
            .lock()
            .map_err(|_| DbError::connection_failed("keyring lock poisoned"))?
            .remove(secret_ref);
        Ok(())
    }
}

/// A keyring write that times out puts the store on hold, so the rollback
/// of the field written before it is refused. The save still fails, and the
/// error names that field as one whose saved value may have changed, never a
/// value (#411 triage round 3, point 1).
#[test]
fn a_refused_rollback_names_each_field_that_may_have_changed() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_two_secrets("old-token", "old-secret");
    store(&state, &mut profile);
    state.add_profile_in_folder(profile.clone(), None);

    let cooldown = CooldownKeyring {
        values: keyring.values.clone(),
        ..CooldownKeyring::default()
    };
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(cooldown));
    let mut edited = profile_with_two_secrets("new-token", "new-secret");
    edited.id = profile.id;
    edited.keyring_secret_fields = profile.keyring_secret_fields.clone();

    let error = state
        .prepare_external_secret_fields(&mut edited)
        .apply()
        .expect_err("the second field times out");

    let message = error.to_string();
    assert_eq!(error.label, "Client secret");
    assert!(message.contains("may have changed"), "{message}");
    assert!(message.contains("API token"), "{message}");
    for value in ["old-token", "new-token", "old-secret", "new-secret"] {
        assert!(!message.contains(value), "{message}");
    }
}

/// An external profile with no driver id keeps no id when it is saved: the
/// built-in driver its kind falls back to says nothing about its values. It
/// reloads with an unknown form and keeps its values (#411 triage round 3,
/// point 3).
#[test]
fn a_profile_without_a_driver_id_reloads_without_one_and_keeps_its_values() {
    let builtin_id = ConnectionProfile::builtin_driver_id_for_kind(datatab_core::DbKind::SQLite);
    let fake = drivers()
        .remove("rpc:fake")
        .expect("the fake driver is registered");
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(HashMap::from([(builtin_id.to_string(), fake)]), &keyring);
    let mut profile = profile_with_token(SENTINEL);
    profile.driver_id = None;
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&profile),
        &std::collections::HashSet::new(),
    )
    .expect("save the profile");

    state.facade.profiles.profiles = crate::config_loader::load_config(&state.storage_runtime)
        .expect("load")
        .profiles;
    state.migrate_external_secret_values();

    let reloaded = &state.profiles()[0];
    assert_eq!(reloaded.driver_id, None);
    assert_eq!(
        external_values(reloaded)
            .get("api_token")
            .map(String::as_str),
        Some(SENTINEL)
    );
    assert!(keyring.values.lock().expect("keyring lock").is_empty());
}

/// A built-in profile with no driver id is still saved with the built-in id
/// its kind falls back to, as before.
#[test]
fn a_built_in_profile_without_a_driver_id_reloads_with_its_built_in_id() {
    let state = state_with_keyring(drivers(), &MemoryKeyring::default());
    let mut profile = ConnectionProfile::new("built in", DbConfig::default_sqlite());
    profile.driver_id = None;
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&profile),
        &std::collections::HashSet::new(),
    )
    .expect("save the profile");

    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");

    assert_eq!(
        reloaded.profiles[0].driver_id.as_deref(),
        Some(ConnectionProfile::builtin_driver_id_for_kind(
            datatab_core::DbKind::SQLite
        ))
    );
}

const PASSWORD_SENTINEL: &str = "sentinel-password-literal-52c8d4";

fn password_ref(profile: &ConnectionProfile) -> String {
    datatab_core::connection_secret_ref(&profile.id)
}

/// A literal value ref on the primary `password` is a secret too: a save
/// never writes it to the database (#415).
#[test]
fn a_literal_password_ref_never_reaches_the_database_on_save() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("");
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );
    profile.value_refs.insert(
        "host".into(),
        datatab_core::ValueRef::literal("plain-host.example"),
    );

    state.add_profile_in_folder(profile, None);

    let bytes = database_bytes(&state.storage_runtime);
    assert!(contains(&bytes, "db.example"), "the database file was read");
    assert!(!contains(&bytes, PASSWORD_SENTINEL));
    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(reloaded.profiles[0].value_refs.contains_key("host"));
    assert!(!reloaded.profiles[0].value_refs.contains_key("password"));
}

/// A row written before the rule holds the literal. At load it moves to the
/// password slot of the keyring and the row is rewritten without it.
#[test]
fn an_old_literal_password_ref_moves_to_the_password_slot() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut old = profile_with_token("");
    old.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");
    assert!(contains(
        &database_bytes(&state.storage_runtime),
        PASSWORD_SENTINEL
    ));

    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");
    state.facade.profiles.profiles = loaded.profiles;
    state.value_ref_diagnostics = loaded.value_ref_diagnostics;
    state.migrate_external_secret_values();

    assert_eq!(
        keyring.get(&password_ref(&old)).as_deref(),
        Some(PASSWORD_SENTINEL)
    );
    assert!(state.facade.profiles.profiles[0].value_refs.is_empty());
    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(reloaded.profiles[0].value_refs.is_empty());
    assert_no_stored_literal(&state);
}

/// A deleted row may linger in a free page of the file, so the check reads
/// the rows back, as the next start would.
fn assert_no_stored_literal(state: &AppState) {
    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(reloaded.profiles[0].value_refs.is_empty());
}

/// The user chose not to save the password: the literal is dropped, never
/// stored anywhere.
#[test]
fn an_old_literal_password_ref_is_dropped_when_the_password_is_not_saved() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut old = profile_with_token("");
    old.save_password = false;
    old.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");

    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");
    state.facade.profiles.profiles = loaded.profiles;
    state.value_ref_diagnostics = loaded.value_ref_diagnostics;
    state.migrate_external_secret_values();

    assert_eq!(keyring.get(&password_ref(&old)), None);
    assert_no_stored_literal(&state);
}

/// A refused keyring write must not leave the literal in the database.
#[test]
fn an_old_literal_password_ref_is_dropped_when_the_keyring_is_locked() {
    let keyring = MemoryKeyring {
        locked: true,
        ..MemoryKeyring::default()
    };
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut old = profile_with_token("");
    old.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");

    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");
    state.facade.profiles.profiles = loaded.profiles;
    state.value_ref_diagnostics = loaded.value_ref_diagnostics;
    state.migrate_external_secret_values();

    assert_no_stored_literal(&state);
}

/// A literal on a field that is not secret keeps today's behavior.
#[test]
fn a_literal_ref_on_a_plain_field_is_still_stored() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("");
    profile.value_refs.insert(
        "host".into(),
        datatab_core::ValueRef::literal("plain-host.example"),
    );

    state.add_profile_in_folder(profile, None);

    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(reloaded.profiles[0].value_refs.contains_key("host"));
}

/// Loads the stored profiles into the state and runs the load migration, as
/// a start does.
fn restart(state: &mut AppState) {
    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("load");
    state.facade.profiles.profiles = loaded.profiles;
    state.value_ref_diagnostics = loaded.value_ref_diagnostics;
    state.migrate_external_secret_values();
}

#[test]
fn undeclared_value_refs_are_dropped_on_load_and_save() {
    assert_undeclared_value_ref_is_dropped(false);
}

#[test]
fn undeclared_value_refs_are_deleted_beside_a_broken_declared_ref() {
    assert_undeclared_value_ref_is_dropped(true);
}

fn assert_undeclared_value_ref_is_dropped(broken_host: bool) {
    let mut state = state_with_keyring(drivers(), &MemoryKeyring::default());
    let mut old = profile_with_token("");
    old.value_refs.insert(
        "removed_field".into(),
        datatab_core::ValueRef::env("UNUSED_REMOVED_FIELD"),
    );
    old.value_refs
        .insert("host".into(), datatab_core::ValueRef::env("UNUSED_HOST"));
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");
    if broken_host {
        state
            .storage_runtime
            .open_datatab_db()
            .expect("database")
            .execute(
                "UPDATE cfg_connection_profile_value_refs SET ref_kind = 'retired-kind' \
                 WHERE profile_id = ?1 AND ref_key = 'host'",
                [old.id.to_string()],
            )
            .expect("break the declared host reference");
    }

    restart(&mut state);

    let expected = if broken_host {
        serde_json::json!({})
    } else {
        serde_json::json!({"host": {"source": "env", "key": "UNUSED_HOST"}})
    };
    assert_eq!(value_refs_json(&state.profiles()[0]), expected);
    assert_eq!(
        state.value_ref_diagnostics.contains_key(&old.id),
        broken_host
    );
    state.save_profiles();

    let rows = state
        .storage_runtime
        .connection_profiles()
        .value_refs()
        .get_for_profile(&old.id.to_string())
        .expect("read stored refs");
    let keys: Vec<_> = rows.iter().map(|row| row.ref_key.as_str()).collect();
    assert_eq!(keys, vec!["host"]);
    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert_eq!(value_refs_json(&reloaded.profiles[0]), expected);
    assert_eq!(
        reloaded.value_ref_diagnostics.contains_key(&old.id),
        broken_host
    );
}

#[test]
fn undeclared_selectors_survive_load_and_save() {
    let mut form = FORM.clone();
    for tab in &mut form.tabs {
        for section in &mut tab.sections {
            section
                .fields
                .retain(|field| matches!(field.id.as_str(), "api_token" | "client_secret"));
        }
    }
    let mut state = state_with_keyring(drivers_with_form(form), &MemoryKeyring::default());
    let mut old = profile_with_token("");
    let DbConfig::External { values, .. } = &mut old.config else {
        panic!("not an external config");
    };
    values.clear();
    for field in ["host", "database", "user", "password"] {
        values.insert(field.into(), format!("unused-{field}-value"));
        old.value_refs
            .insert(field.into(), datatab_core::ValueRef::env("UNUSED_SOURCE"));
    }
    let expected_values = values.clone();
    values.insert("removed_field".into(), "unused-removed-value".into());
    let expected_refs = value_refs_json(&old);
    old.value_refs.insert(
        "removed_field".into(),
        datatab_core::ValueRef::env("UNUSED_REMOVED_SOURCE"),
    );
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the row");

    restart(&mut state);

    assert_eq!(value_refs_json(&state.profiles()[0]), expected_refs);
    assert_eq!(external_values(&state.profiles()[0]), &expected_values);
    state.save_profiles();
    assert_eq!(value_refs_json(&state.profiles()[0]), expected_refs);
    assert_eq!(external_values(&state.profiles()[0]), &expected_values);

    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert_eq!(value_refs_json(&reloaded.profiles[0]), expected_refs);
    assert_eq!(external_values(&reloaded.profiles[0]), &expected_values);
    let rows = state
        .storage_runtime
        .connection_profiles()
        .value_refs()
        .get_for_profile(&old.id.to_string())
        .expect("read stored refs");
    let mut keys: Vec<_> = rows.iter().map(|row| row.ref_key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["database", "host", "password", "user"]);
}

#[test]
fn undeclared_value_refs_migration_keeps_declared_refs() {
    let mut state = state_with_keyring(drivers(), &MemoryKeyring::default());
    let mut old = profile_with_token("");
    for field in ["host", "password", "api_token", "client_secret"] {
        old.value_refs
            .insert(field.into(), datatab_core::ValueRef::env("UNUSED_SOURCE"));
    }
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the row");

    restart(&mut state);
    assert_eq!(value_refs_json(&state.profiles()[0]), value_refs_json(&old));
    state.save_profiles();

    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert_eq!(
        value_refs_json(&reloaded.profiles[0]),
        value_refs_json(&old)
    );
}

#[test]
fn undeclared_value_refs_migration_keeps_refs_when_the_form_is_unknown() {
    let mut state = state_with_keyring(HashMap::new(), &MemoryKeyring::default());
    let mut old = profile_with_token("");
    for field in ["host", "password", "api_token", "removed_field"] {
        old.value_refs
            .insert(field.into(), datatab_core::ValueRef::env("UNUSED_SOURCE"));
    }
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the row");

    restart(&mut state);
    assert_eq!(value_refs_json(&state.profiles()[0]), value_refs_json(&old));
    state.save_profiles();

    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert_eq!(
        value_refs_json(&reloaded.profiles[0]),
        value_refs_json(&old)
    );
}

fn value_refs_json(profile: &ConnectionProfile) -> serde_json::Value {
    serde_json::to_value(&profile.value_refs).expect("serialize value refs")
}

struct ThreadLogCapture;

thread_local! {
    static CAPTURED_LOGS: std::cell::RefCell<Option<Vec<String>>> = const {
        std::cell::RefCell::new(None)
    };
}

impl log::Log for ThreadLogCapture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        CAPTURED_LOGS.with(|logs| {
            if let Some(logs) = logs.borrow_mut().as_mut() {
                logs.push(record.args().to_string());
            }
        });
    }

    fn flush(&self) {}
}

pub(super) fn capture_logs(action: impl FnOnce()) -> Vec<String> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        log::set_logger(&ThreadLogCapture).expect("test logger");
        log::set_max_level(log::LevelFilter::Trace);
    });
    CAPTURED_LOGS.with(|logs| *logs.borrow_mut() = Some(Vec::new()));
    action();
    CAPTURED_LOGS.with(|logs| logs.borrow_mut().take().expect("captured logs"))
}

fn stored_refs_json(state: &AppState, profile: &ConnectionProfile) -> serde_json::Value {
    let rows = state
        .storage_runtime
        .connection_profiles()
        .value_refs()
        .get_for_profile(&profile.id.to_string())
        .expect("stored refs");
    serde_json::to_value(rows).expect("serialize rows")
}

#[test]
fn unknown_form_save_preserves_stored_refs_exactly() {
    for without_id in [false, true] {
        for broken_ref in [false, true] {
            let mut state = state_with_keyring(HashMap::new(), &MemoryKeyring::default());
            let mut old = profile_with_token("");
            if without_id {
                old.driver_id = None;
            }
            old.value_refs
                .insert("password".into(), ValueRef::literal(PASSWORD_SENTINEL));
            old.value_refs
                .insert("host".into(), ValueRef::env("UNUSED_HOST"));
            crate::config_loader::save_profiles(
                &state.storage_runtime,
                std::slice::from_ref(&old),
                &std::collections::HashSet::new(),
            )
            .expect("seed stored refs");
            if broken_ref {
                state.storage_runtime.open_datatab_db().expect("database").execute(
            "UPDATE cfg_connection_profile_value_refs SET ref_kind = 'retired-kind' WHERE ref_key = 'host'",
            [],
        ).expect("seed broken ref");
            }
            if without_id {
                state.storage_runtime.open_datatab_db().expect("database").execute(
                "UPDATE cfg_connection_profile_value_refs SET literal_value = NULL WHERE ref_key = 'password'",
                [],
            ).expect("seed legacy literal column");
            }
            let expected = stored_refs_json(&state, &old);
            restart(&mut state);
            state.save_profiles();
            assert_eq!(stored_refs_json(&state, &old), expected);
            state.facade.profiles.profiles[0].value_refs.clear();
            state.facade.profiles.profiles[0].name = "renamed".into();

            state.save_profiles();

            assert_eq!(stored_refs_json(&state, &old), expected);
            let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
            assert_eq!(loaded.profiles[0].name, "renamed");
        }
    }
}

#[test]
fn unknown_form_save_refuses_new_literals_before_any_write() {
    for without_id in [false, true] {
        for replace_stored in [false, true] {
            let mut state = state_with_keyring(HashMap::new(), &MemoryKeyring::default());
            let mut old = profile_with_token("");
            if without_id {
                old.driver_id = None;
            }
            old.value_refs
                .insert("password".into(), ValueRef::literal("old-literal"));
            crate::config_loader::save_profiles(
                &state.storage_runtime,
                std::slice::from_ref(&old),
                &std::collections::HashSet::new(),
            )
            .expect("seed profile");
            let expected = stored_refs_json(&state, &old);
            restart(&mut state);
            state.facade.profiles.profiles[0].name = "must not be written".into();
            state.facade.profiles.profiles[0].value_refs.insert(
                if replace_stored {
                    "password"
                } else {
                    "new_field"
                }
                .into(),
                ValueRef::literal(PASSWORD_SENTINEL),
            );
            let mut new_profile = profile_with_token("");
            new_profile.name = "also must not be written".into();
            state.facade.profiles.profiles.insert(0, new_profile);

            let logs = capture_logs(|| state.save_profiles());

            let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
            assert_eq!(loaded.profiles.len(), 1, "refusal must precede all writes");
            assert_eq!(loaded.profiles[0].name, old.name);
            assert_eq!(stored_refs_json(&state, &old), expected);
            assert!(logs.iter().any(|line| line.contains("literal") && line.contains("registered driver form")));
            assert!(logs.iter().all(|line| !line.contains(PASSWORD_SENTINEL)
                && !line.contains("new_field")
                && !line.contains("must not be written")));
        }
    }
}

#[test]
fn save_settles_literals_without_clearing_typed_external_values_in_memory() {
    let mut state = state_with_keyring(drivers(), &MemoryKeyring::default());
    let mut profile = profile_with_token(SENTINEL);
    profile.save_password = false;
    profile
        .value_refs
        .insert("password".into(), ValueRef::literal(PASSWORD_SENTINEL));
    profile.value_refs.insert(
        "client_secret".into(),
        ValueRef::literal("extra-literal-433"),
    );
    state.facade.profiles.profiles.push(profile);

    state.save_profiles();

    assert_eq!(
        external_values(&state.profiles()[0])
            .get("api_token")
            .map(String::as_str),
        Some(SENTINEL)
    );
    assert!(state.profiles()[0].value_refs.is_empty());
    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(loaded.profiles[0].value_refs.is_empty());
    assert!(!external_values(&loaded.profiles[0]).contains_key("api_token"));
    assert!(!contains(&database_bytes(&state.storage_runtime), SENTINEL));
}

#[test]
fn two_saves_settle_a_literal_secret_once_in_memory_and_logs() {
    for (save_password, existing_password) in [(false, false), (true, true), (true, false)] {
        let keyring = MemoryKeyring::default();
        let mut state = state_with_keyring(drivers(), &keyring);
        let mut profile = profile_with_token("");
        profile.save_password = save_password;
        profile
            .value_refs
            .insert("password".into(), ValueRef::literal(PASSWORD_SENTINEL));
        if existing_password {
            seed_password(&keyring, &profile, "existing-password");
        }
        state.facade.profiles.profiles.push(profile.clone());

        let logs = capture_logs(|| {
            state.save_profiles();
            state.save_profiles();
        });

        let secret_logs: Vec<_> = logs
            .iter()
            .filter(|line| line.contains("secret value(s)"))
            .collect();
        assert_eq!(
            secret_logs.len(),
            1,
            "a second save must not settle the literal again"
        );
        assert!(
            !secret_logs[0].contains("stored"),
            "a save can settle an unsaved literal"
        );
        assert_eq!(
            secret_logs[0].starts_with("Dropped 1"),
            !save_password || existing_password
        );
        assert!(
            secret_logs
                .iter()
                .all(|line| !line.contains(PASSWORD_SENTINEL)
                    && !line.contains(&profile.name)
                    && !line.contains("password'"))
        );
        assert!(logs.iter().all(|line| !line.contains(PASSWORD_SENTINEL)));
        assert!(state.profiles()[0].value_refs.is_empty());
        assert_no_stored_literal(&state);
        let expected = if existing_password {
            Some("existing-password")
        } else if save_password {
            Some(PASSWORD_SENTINEL)
        } else {
            None
        };
        assert_eq!(keyring.get(&password_ref(&profile)).as_deref(), expected);
    }
}

/// A password that comes from another source is not a literal: the start and
/// a save leave it alone (#415 round 1, point 1).
#[test]
fn a_non_literal_password_ref_survives_startup_and_a_save() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut old = profile_with_token("");
    old.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::env("DB_PASSWORD"),
    );
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the row");

    restart(&mut state);
    assert!(matches!(
        state.facade.profiles.profiles[0].value_refs.get("password"),
        Some(datatab_core::ValueRef::Env { .. })
    ));
    state.save_profiles();

    let reloaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert!(matches!(
        reloaded.profiles[0].value_refs.get("password"),
        Some(datatab_core::ValueRef::Env { .. })
    ));
}

fn seed_password(keyring: &MemoryKeyring, profile: &ConnectionProfile, value: &str) {
    keyring
        .values
        .lock()
        .expect("keyring lock")
        .insert(password_ref(profile), value.to_string());
}

/// The slot already holds a password: the load keeps it and drops the
/// literal (#415 round 1, point 2).
#[test]
fn a_literal_password_never_overwrites_a_saved_password_at_load() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut old = profile_with_token("");
    old.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );
    seed_password(&keyring, &old, "saved-password");
    crate::config_loader::save_profiles(
        &state.storage_runtime,
        std::slice::from_ref(&old),
        &std::collections::HashSet::new(),
    )
    .expect("write the old-style row");

    restart(&mut state);

    assert_eq!(
        keyring.get(&password_ref(&old)).as_deref(),
        Some("saved-password")
    );
    assert_no_stored_literal(&state);
}

/// A save moves a literal password to an empty slot and survives a restart
/// (#415 round 1, point 3).
#[test]
fn a_save_moves_a_literal_password_to_an_empty_slot() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("");
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );

    state.add_profile_in_folder(profile.clone(), None);
    restart(&mut state);

    assert_eq!(
        keyring.get(&password_ref(&profile)).as_deref(),
        Some(PASSWORD_SENTINEL)
    );
    assert_no_stored_literal(&state);
}

/// A save keeps a saved password that differs, and drops the literal.
#[test]
fn a_save_keeps_a_different_saved_password() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("");
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );
    seed_password(&keyring, &profile, "saved-password");

    state.add_profile_in_folder(profile.clone(), None);
    restart(&mut state);

    assert_eq!(
        keyring.get(&password_ref(&profile)).as_deref(),
        Some("saved-password")
    );
    assert_no_stored_literal(&state);
}

/// The user does not save the password: a save drops the literal and writes
/// nothing to the keyring.
#[test]
fn a_save_drops_a_literal_password_when_the_password_is_not_saved() {
    let keyring = MemoryKeyring::default();
    let mut state = state_with_keyring(drivers(), &keyring);
    let mut profile = profile_with_token("");
    profile.save_password = false;
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );

    state.add_profile_in_folder(profile.clone(), None);
    restart(&mut state);

    assert_eq!(keyring.get(&password_ref(&profile)), None);
    assert_no_stored_literal(&state);
}

#[test]
fn unknown_form_manager_check_refuses_new_literals_without_mutation() {
    for without_id in [false, true] {
        for field in ["password", "new_field"] {
            let keyring = MemoryKeyring::default();
            let mut state = state_with_keyring(HashMap::new(), &keyring);
            let mut stored = profile_with_token("");
            if without_id {
                stored.driver_id = None;
            }
            stored
                .value_refs
                .insert("password".into(), ValueRef::literal("old-literal"));
            crate::config_loader::save_profiles(
                &state.storage_runtime,
                std::slice::from_ref(&stored),
                &std::collections::HashSet::new(),
            )
            .expect("seed profile");
            restart(&mut state);
            let expected_rows = stored_refs_json(&state, &stored);
            let expected_memory = value_refs_json(&state.profiles()[0]);
            assert!(
                state
                    .check_connection_manager_save(&stored, "typed")
                    .is_ok()
            );
            let mut edited = stored.clone();
            edited.name = "refused edit".into();
            edited
                .value_refs
                .insert(field.into(), ValueRef::literal(SENTINEL));

            let result = state.check_connection_manager_save(&edited, "typed");

            assert!(
                result.is_err(),
                "a new literal on an unknown form must be refused"
            );
            assert!(
                state
                    .update_profile_from_connection_manager(edited, "typed")
                    .is_err()
            );
            assert_eq!(stored_refs_json(&state, &stored), expected_rows);
            assert_eq!(value_refs_json(&state.profiles()[0]), expected_memory);
            assert_eq!(state.profiles()[0].name, stored.name);
            assert!(keyring.values.lock().expect("keyring").is_empty());
        }
    }
}

#[test]
fn unknown_form_refusal_is_not_a_storage_data_error() {
    let mut state = state_with_keyring(HashMap::new(), &MemoryKeyring::default());
    for name in ["first private profile", "second private profile"] {
        let mut profile = profile_with_token("");
        profile.name = name.into();
        profile
            .value_refs
            .insert("private_field".into(), ValueRef::literal(SENTINEL));
        state.facade.profiles.profiles.push(profile);
    }
    let error = state
        .value_ref_preservation_for_save(
            &state.profiles().iter().map(|profile| profile.id).collect(),
        )
        .expect_err("refuse literals");
    assert_eq!(
        error.to_string(),
        "Save refused for 2 connection profile(s): newly supplied literal refs require a registered driver form"
    );
    let logs = capture_logs(|| state.save_profiles());
    assert_eq!(
        logs,
        vec![
            "Connection profiles were not saved: Save refused for 2 connection profile(s): newly supplied literal refs require a registered driver form"
        ]
    );
}

#[test]
fn save_counts_stripped_external_literal_secrets_once() {
    let mut state = state_with_keyring(drivers(), &MemoryKeyring::default());
    let mut profile = profile_with_token("");
    for field in ["api_token", "client_secret"] {
        profile
            .value_refs
            .insert(field.into(), ValueRef::literal(SENTINEL));
    }
    profile
        .value_refs
        .insert("host".into(), ValueRef::literal("ordinary-host"));
    state.facade.profiles.profiles.push(profile);
    let logs = capture_logs(|| {
        state.save_profiles();
        state.save_profiles();
    });
    let dropped: Vec<_> = logs
        .iter()
        .filter(|line| line.starts_with("Dropped "))
        .collect();
    assert_eq!(
        dropped.len(),
        1,
        "both stripped refs must be counted only on the first save"
    );
    assert!(dropped[0].starts_with("Dropped 2 secret value(s):"));
    assert!(logs.iter().all(|line| !line.contains(SENTINEL)
        && !line.contains("api_token")
        && !line.contains("client_secret")
        && !line.contains("ordinary-host")));
    assert_eq!(state.profiles()[0].value_refs.len(), 1);
    let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
    assert_eq!(loaded.profiles[0].value_refs.len(), 1);
}

#[test]
fn save_drop_log_explains_empty_literals_and_every_other_drop_reason() {
    let mut state = state_with_keyring(drivers(), &MemoryKeyring::default());
    let mut profile = profile_with_token("");
    profile.save_password = true;
    profile
        .value_refs
        .insert("password".into(), ValueRef::literal(""));
    state.facade.profiles.profiles.push(profile);
    let logs = capture_logs(|| state.save_profiles());
    let dropped: Vec<_> = logs
        .iter()
        .filter(|line| line.starts_with("Dropped "))
        .map(String::as_str)
        .collect();
    assert_eq!(
        dropped,
        vec![
            "Dropped 1 secret value(s): the literal is empty, the slot already holds a value, no usable keyring slot is available, password saving is disabled, another value source is selected, or a non-password literal secret ref was stripped"
        ]
    );
}

#[test]
fn known_form_new_secret_literal_passes_manager_check_and_saves() {
    for field in ["password", "api_token"] {
        let keyring = MemoryKeyring::default();
        let mut state = state_with_keyring(drivers(), &keyring);
        let mut profile = profile_with_token("");
        profile.save_password = true;
        profile
            .value_refs
            .insert(field.into(), ValueRef::literal(SENTINEL));
        state
            .check_connection_manager_save(&profile, "typed")
            .expect("a known form accepts a new literal before settlement");
        state.facade.profiles.profiles.push(profile.clone());

        state.save_profiles();

        let loaded = crate::config_loader::load_config(&state.storage_runtime).expect("reload");
        assert_eq!(loaded.profiles.len(), 1, "the save must write the profile");
        assert_eq!(loaded.profiles[0].id, profile.id);
        assert!(state.profiles()[0].value_refs.is_empty());
        assert!(loaded.profiles[0].value_refs.is_empty());
        assert!(!contains(&database_bytes(&state.storage_runtime), SENTINEL));
        if field == "password" {
            assert_eq!(
                keyring.get(&password_ref(&profile)).as_deref(),
                Some(SENTINEL)
            );
        } else {
            assert_eq!(
                keyring.get(&field_ref(&profile)),
                None,
                "non-password literal secret refs are dropped, as #415 decided"
            );
        }
    }
}
