use super::super::AppStatePersistence;
use datatab_app::portability::{ConnectionInsertResult, ImportPersistence};
use datatab_core::secrecy::{ExposeSecret, SecretString};
use datatab_core::{
    ConnectionProfile, DbConfig, DbDriver, DbError, DbKind, DriverFormDef, FormValues, SecretStore,
};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

const SENTINEL: &str = "sentinel-api-token-7f3a91";

static FORM: LazyLock<DriverFormDef> = LazyLock::new(|| {
    use datatab_core::{FormFieldKind, FormSection, FormTab, field};
    DriverFormDef {
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

struct RpcDriver(datatab_core::DriverMetadata);

impl DbDriver for RpcDriver {
    fn kind(&self) -> DbKind {
        DbKind::SQLite
    }
    fn metadata(&self) -> &datatab_core::DriverMetadata {
        &self.0
    }
    fn form_definition(&self) -> &DriverFormDef {
        &FORM
    }
    fn driver_key(&self) -> datatab_core::DriverKey {
        "rpc:import".into()
    }
    fn build_config(&self, values: &FormValues) -> Result<DbConfig, DbError> {
        Ok(DbConfig::External {
            kind: DbKind::SQLite,
            values: values.clone(),
        })
    }
    fn extract_values(&self, config: &DbConfig) -> FormValues {
        match config {
            DbConfig::External { values, .. } => values.clone(),
            _ => FormValues::new(),
        }
    }
    fn connect_with_secrets(
        &self,
        _: &ConnectionProfile,
        _: Option<&SecretString>,
        _: Option<&SecretString>,
    ) -> Result<Box<dyn datatab_core::Connection>, DbError> {
        Err(DbError::NotSupported("import test driver".into()))
    }
    fn test_connection(&self, _: &ConnectionProfile) -> Result<(), DbError> {
        Ok(())
    }
}

/// An in-memory keyring. `locked` makes every write fail; `fail_field`
/// makes the writes of one field fail; `fail_password` makes the writes of
/// connection password slots fail.
#[derive(Clone, Default)]
struct Keyring {
    values: Arc<Mutex<HashMap<String, String>>>,
    locked: bool,
    fail_field: Option<&'static str>,
    fail_password: bool,
    fail_delete: Arc<std::sync::atomic::AtomicBool>,
    refuse_field_deletes: Arc<std::sync::atomic::AtomicBool>,
}

impl Keyring {
    fn entry(&self, secret_ref: &str) -> Option<String> {
        self.values
            .lock()
            .expect("keyring")
            .get(secret_ref)
            .cloned()
    }
}

impl SecretStore for Keyring {
    fn is_available(&self) -> bool {
        true
    }
    fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, DbError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| DbError::connection_failed("fixture lock poisoned"))?
            .get(secret_ref)
            .cloned()
            .map(SecretString::from))
    }
    fn set(&self, secret_ref: &str, value: &SecretString) -> Result<(), DbError> {
        let failing_field = self
            .fail_field
            .is_some_and(|field| secret_ref.ends_with(&format!(":field:{field}")));
        let password_slot = self.fail_password && !secret_ref.contains(":field:");
        if self.locked || failing_field || password_slot {
            return Err(DbError::IoError(std::io::Error::other("keyring locked")));
        }
        self.values
            .lock()
            .map_err(|_| DbError::connection_failed("fixture lock poisoned"))?
            .insert(secret_ref.to_string(), value.expose_secret().to_string());
        Ok(())
    }
    fn delete(&self, secret_ref: &str) -> Result<(), DbError> {
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst)
            || (secret_ref.contains(":field:")
                && self
                    .refuse_field_deletes
                    .load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(DbError::connection_failed("delete refused"));
        }
        self.values
            .lock()
            .map_err(|_| DbError::connection_failed("fixture lock poisoned"))?
            .remove(secret_ref);
        Ok(())
    }
}

fn state(keyring: &Keyring) -> datatab_app::AppState {
    let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage runtime");
    let mut state = datatab_app::AppState::new_with_storage_runtime(runtime).expect("app state");
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(keyring.clone()));
    let metadata = datatab_core::DriverMetadataBuilder::new(
        "import",
        "Import",
        datatab_core::DatabaseCategory::Relational,
        datatab_core::QueryLanguage::Sql,
    )
    .build();
    state.register_driver_for_test("rpc:import", Arc::new(RpcDriver(metadata)));
    state
}

/// A bundle profile whose secret field holds a value, as a config
/// value or as a literal reference.
fn bundled_profiles() -> Vec<ConnectionProfile> {
    let config = |token: &str| DbConfig::External {
        kind: DbKind::SQLite,
        values: HashMap::from([
            ("host".to_string(), "db.example".to_string()),
            ("api_token".to_string(), token.to_string()),
        ]),
    };
    let in_config = ConnectionProfile::new_with_driver(
        "in config",
        DbKind::SQLite,
        "rpc:import",
        config(SENTINEL),
    );
    let mut as_literal =
        ConnectionProfile::new_with_driver("as literal", DbKind::SQLite, "rpc:import", config(""));
    as_literal.value_refs.insert(
        "api_token".into(),
        datatab_core::ValueRef::literal(SENTINEL),
    );
    vec![in_config, as_literal]
}

#[test]
fn an_imported_secret_field_value_goes_to_the_keyring() {
    for profile in bundled_profiles() {
        let keyring = Keyring::default();
        let mut state = state(&keyring);
        let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");

        let result = AppStatePersistence::new(&mut state).add_connection(profile);

        assert!(
            matches!(result, ConnectionInsertResult::Ok),
            "{}",
            state.profiles()[0].name
        );
        let imported = &state.profiles()[0];
        assert!(imported.keyring_secret_fields.contains("api_token"));
        assert!(!format!("{imported:?}").contains(SENTINEL));
        assert_eq!(
            keyring
                .values
                .lock()
                .expect("keyring")
                .get(&secret_ref)
                .map(String::as_str),
            Some(SENTINEL)
        );
    }
}

#[test]
fn an_import_the_keyring_refuses_fails_with_a_reason() {
    for profile in bundled_profiles() {
        let keyring = Keyring {
            locked: true,
            ..Keyring::default()
        };
        let mut state = state(&keyring);

        let result = AppStatePersistence::new(&mut state).add_connection(profile);

        match result {
            ConnectionInsertResult::ConfigFailed(reason) => {
                assert!(reason.contains("API token"), "{reason}");
                assert!(!reason.contains(SENTINEL));
            }
            _ => panic!("the import must fail"),
        }
        assert!(state.profiles().is_empty());
    }
}

/// An import whose second secret field cannot be written fails and leaves
/// no entry for the first one (#411 triage round 2, point 1).
#[test]
fn a_refused_import_leaves_no_keyring_entry() {
    let keyring = Keyring {
        fail_field: Some("client_secret"),
        ..Keyring::default()
    };
    let mut state = state(&keyring);
    let profile = ConnectionProfile::new_with_driver(
        "two secrets",
        DbKind::SQLite,
        "rpc:import",
        DbConfig::External {
            kind: DbKind::SQLite,
            values: HashMap::from([
                ("api_token".to_string(), SENTINEL.to_string()),
                ("client_secret".to_string(), "second".to_string()),
            ]),
        },
    );
    let token_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");

    let result = AppStatePersistence::new(&mut state).add_connection(profile);

    match result {
        ConnectionInsertResult::ConfigFailed(reason) => {
            assert!(reason.contains("Client secret"), "{reason}");
        }
        _ => panic!("the import must fail"),
    }
    assert!(state.profiles().is_empty());
    assert_eq!(keyring.entry(&token_ref), None);
}

/// Reads every secret from the keyring, so the export below could stage
/// one if it tried.
struct KeyringReader(Keyring);

impl datatab_portability::SecretReader for KeyringReader {
    fn read(&self, secret_ref: &str) -> Option<SecretString> {
        self.0.entry(secret_ref).map(SecretString::from)
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn database_bytes(state: &datatab_app::AppState) -> Vec<u8> {
    let path = state.storage_runtime().datatab_db_path().to_path_buf();
    let mut bytes = std::fs::read(&path).unwrap_or_default();
    let mut wal = path.into_os_string();
    wal.push("-wal");
    bytes.extend(std::fs::read(wal).unwrap_or_default());
    bytes
}

fn export_bundle(source: &datatab_app::AppState, keyring: &Keyring) -> Vec<u8> {
    use datatab_portability::{
        ConnectionWithValues, EncryptionChoice, ExportGraph, ExportOptions, IncludeExclude,
    };

    let profile = &source.profiles()[0];
    let driver = source
        .drivers()
        .get("rpc:import")
        .cloned()
        .expect("registered driver");
    let graph = ExportGraph {
        connections: vec![ConnectionWithValues {
            profile,
            values: driver.extract_values(&profile.config),
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
        connection_password: IncludeExclude::Include,
        proxy_credentials: IncludeExclude::Exclude,
        ssh_password: IncludeExclude::Exclude,
        auth_modes: Default::default(),
        per_secret_overrides: Default::default(),
    };
    let (bytes, _) = datatab_portability::export::export(
        &graph,
        &opts,
        &datatab_app::portability::AppFieldHintResolver::new(source.drivers().clone()),
        &datatab_app::portability::AppExportTransformResolver::new(source.drivers().clone()),
        &KeyringReader(keyring.clone()),
    )
    .expect("export");
    bytes
}

/// A secret field kept in the keyring is exported as a secret reference,
/// never as a value; the import asks for it and stores the answer in the new
/// profile's own entry for that field, not its password slot and not the
/// database (#411 triage round 2, point 3).
#[test]
fn an_exported_secret_field_is_asked_for_and_imported_into_its_own_entry() {
    let source_keyring = Keyring::default();
    let mut source = state(&source_keyring);
    let mut profile = ConnectionProfile::new_with_driver(
        "exported",
        DbKind::SQLite,
        "rpc:import",
        DbConfig::External {
            kind: DbKind::SQLite,
            values: HashMap::from([
                ("host".to_string(), "db.example".to_string()),
                ("api_token".to_string(), SENTINEL.to_string()),
            ]),
        },
    );
    let _stored = source
        .prepare_external_secret_fields(&mut profile)
        .apply()
        .expect("the keyring takes the value");
    source.add_profile_in_folder(profile, None);

    let bytes = export_bundle(&source, &source_keyring);
    assert!(
        contains(&bytes, "db.example"),
        "the connection was exported"
    );
    assert!(
        !contains(&bytes, SENTINEL),
        "the value never reaches the bundle"
    );

    let parsed = datatab_portability::import::parse(&bytes).expect("parse");
    let plan = datatab_portability::import::plan(
        &parsed,
        &datatab_portability::DestSnapshot {
            auth_profiles: Vec::new(),
            ssh_tunnels: Vec::new(),
            proxies: Vec::new(),
            connections: Vec::new(),
        },
    );
    let asked = plan
        .required_resolutions
        .iter()
        .find(|resolution| resolution.field == "api_token")
        .expect("the import asks for the secret field");
    let mut choices = datatab_portability::ResolutionChoices::default();
    choices.secret_values.insert(
        (asked.owner_local_id.clone(), "api_token".to_string()),
        SecretString::from(SENTINEL),
    );

    let target_keyring = Keyring::default();
    let mut target = state(&target_keyring);
    let outcome = datatab_app::portability::apply_import(
        &parsed,
        &plan,
        &choices,
        &mut AppStatePersistence::new(&mut target),
    )
    .expect("import");

    assert_eq!(outcome.succeeded, ["exported"]);
    assert!(outcome.secret_failures.is_empty());
    let imported = &target.profiles()[0];
    assert!(imported.keyring_secret_fields.contains("api_token"));
    assert_eq!(
        target_keyring
            .entry(&datatab_core::connection_field_secret_ref(
                &imported.id,
                "api_token"
            ))
            .as_deref(),
        Some(SENTINEL)
    );
    assert_eq!(
        target_keyring.entry(&datatab_core::connection_secret_ref(&imported.id)),
        None,
        "the password slot is not written"
    );
    let database = database_bytes(&target);
    assert!(
        contains(&database, "db.example"),
        "the database file was read"
    );
    assert!(!contains(&database, SENTINEL));
}

/// The password slot of an imported external profile is written in the same
/// step as its secret fields: when it cannot be written, the field entries
/// are removed again and the profile is not saved; the connection fails with
/// a reason (#411 triage round 3, point 2).
#[test]
fn an_import_whose_password_cannot_be_written_saves_nothing() {
    let keyring = Keyring {
        fail_password: true,
        ..Keyring::default()
    };
    let mut state = state(&keyring);
    let profile = ConnectionProfile::new_with_driver(
        "with password",
        DbKind::SQLite,
        "rpc:import",
        DbConfig::External {
            kind: DbKind::SQLite,
            values: HashMap::from([
                ("host".to_string(), "db.example".to_string()),
                ("api_token".to_string(), SENTINEL.to_string()),
            ]),
        },
    );
    let token_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");
    let actions = datatab_portability::ImportActions {
        secret_writes: vec![(
            datatab_core::connection_secret_ref(&profile.id),
            SecretString::from("primary-password"),
        )],
        connections: vec![profile],
        auth_profiles: Vec::new(),
        ssh_tunnels: Vec::new(),
        proxies: Vec::new(),
        connection_field_secrets: Vec::new(),
        kind_failures: Vec::new(),
        unresolved_ref_connections: Vec::new(),
        value_ref_failures: Vec::new(),
        chosen_auth_profiles: Vec::new(),
    };

    let outcome = datatab_app::portability::persist_import_actions(
        actions,
        &mut AppStatePersistence::new(&mut state),
    );

    assert!(outcome.succeeded.is_empty(), "{:?}", outcome.succeeded);
    assert_eq!(outcome.config_failures.len(), 1);
    assert_eq!(outcome.config_failures[0].0, "with password");
    assert!(!outcome.config_failures[0].1.contains("primary-password"));
    assert!(state.profiles().is_empty());
    assert_eq!(keyring.entry(&token_ref), None);
    assert!(!contains(&database_bytes(&state), SENTINEL));
}

/// A driver that builds a built-in config from the bundle values.
struct BuiltinDriver(datatab_core::DriverMetadata);

impl DbDriver for BuiltinDriver {
    fn kind(&self) -> DbKind {
        DbKind::SQLite
    }
    fn metadata(&self) -> &datatab_core::DriverMetadata {
        &self.0
    }
    fn form_definition(&self) -> &DriverFormDef {
        &FORM
    }
    fn driver_key(&self) -> datatab_core::DriverKey {
        "builtin-import".into()
    }
    fn build_config(&self, _values: &FormValues) -> Result<DbConfig, DbError> {
        Ok(DbConfig::SQLite {
            path: std::path::PathBuf::from("imported.db"),
            connection_id: None,
        })
    }
    fn extract_values(&self, _config: &DbConfig) -> FormValues {
        FormValues::new()
    }
    fn connect_with_secrets(
        &self,
        _: &ConnectionProfile,
        _: Option<&SecretString>,
        _: Option<&SecretString>,
    ) -> Result<Box<dyn datatab_core::Connection>, DbError> {
        Err(DbError::NotSupported("import test driver".into()))
    }
    fn test_connection(&self, _: &ConnectionProfile) -> Result<(), DbError> {
        Ok(())
    }
}

/// A built-in profile keeps the old path: it is saved, and a password the
/// keyring refuses is reported as a secret failure, not a failed connection.
#[test]
fn a_built_in_import_whose_password_cannot_be_written_is_still_saved() {
    let keyring = Keyring {
        fail_password: true,
        ..Keyring::default()
    };
    let mut state = state(&keyring);
    let metadata = datatab_core::DriverMetadataBuilder::new(
        "builtin-import",
        "Built-in import",
        datatab_core::DatabaseCategory::Relational,
        datatab_core::QueryLanguage::Sql,
    )
    .build();
    state.register_driver_for_test("builtin-import", Arc::new(BuiltinDriver(metadata)));
    let profile = ConnectionProfile::new_with_driver(
        "built in",
        DbKind::SQLite,
        "builtin-import",
        DbConfig::External {
            kind: DbKind::SQLite,
            values: HashMap::from([("host".to_string(), "db.example".to_string())]),
        },
    );
    let actions = datatab_portability::ImportActions {
        secret_writes: vec![(
            datatab_core::connection_secret_ref(&profile.id),
            SecretString::from("primary-password"),
        )],
        connections: vec![profile],
        auth_profiles: Vec::new(),
        ssh_tunnels: Vec::new(),
        proxies: Vec::new(),
        connection_field_secrets: Vec::new(),
        kind_failures: Vec::new(),
        unresolved_ref_connections: Vec::new(),
        value_ref_failures: Vec::new(),
        chosen_auth_profiles: Vec::new(),
    };

    let outcome = datatab_app::portability::persist_import_actions(
        actions,
        &mut AppStatePersistence::new(&mut state),
    );

    assert_eq!(outcome.succeeded, ["built in"]);
    assert!(
        outcome.config_failures.is_empty(),
        "{:?}",
        outcome.config_failures
    );
    assert_eq!(outcome.secret_failures.len(), 1);
    assert_eq!(state.profiles().len(), 1);
}

/// An external profile whose import wrote no secret-field entry has nothing
/// to roll back: it is saved, and a refused password is reported as a
/// secret failure, as before.
#[test]
fn an_external_import_without_a_field_entry_keeps_a_refused_password_as_a_secret_failure() {
    let keyring = Keyring {
        fail_password: true,
        ..Keyring::default()
    };
    let mut state = state(&keyring);
    let profile = ConnectionProfile::new_with_driver(
        "no secret fields",
        DbKind::SQLite,
        "rpc:import",
        DbConfig::External {
            kind: DbKind::SQLite,
            values: HashMap::from([("host".to_string(), "db.example".to_string())]),
        },
    );
    let actions = datatab_portability::ImportActions {
        secret_writes: vec![(
            datatab_core::connection_secret_ref(&profile.id),
            SecretString::from("primary-password"),
        )],
        connections: vec![profile],
        auth_profiles: Vec::new(),
        ssh_tunnels: Vec::new(),
        proxies: Vec::new(),
        connection_field_secrets: Vec::new(),
        kind_failures: Vec::new(),
        unresolved_ref_connections: Vec::new(),
        value_ref_failures: Vec::new(),
        chosen_auth_profiles: Vec::new(),
    };

    let outcome = datatab_app::portability::persist_import_actions(
        actions,
        &mut AppStatePersistence::new(&mut state),
    );

    assert_eq!(outcome.succeeded, ["no secret fields"]);
    assert!(
        outcome.config_failures.is_empty(),
        "{:?}",
        outcome.config_failures
    );
    assert_eq!(outcome.secret_failures.len(), 1);
    assert_eq!(state.profiles().len(), 1);
}

const PASSWORD_SENTINEL: &str = "sentinel-password-literal-52c8d4";

/// A bundle that carries the password as a literal value ref: the import
/// writes it to the password slot and the database never holds it (#415).
#[test]
fn an_imported_literal_password_ref_goes_to_the_password_slot() {
    let keyring = Keyring::default();
    let mut state = state(&keyring);
    let mut profile = ConnectionProfile::new_with_driver(
        "literal password",
        DbKind::SQLite,
        "rpc:import",
        DbConfig::External {
            kind: DbKind::SQLite,
            values: HashMap::from([("host".to_string(), "db.example".to_string())]),
        },
    );
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::literal(PASSWORD_SENTINEL),
    );
    let password_ref = datatab_core::connection_secret_ref(&profile.id);
    let actions = datatab_portability::ImportActions {
        connections: vec![profile],
        auth_profiles: Vec::new(),
        ssh_tunnels: Vec::new(),
        proxies: Vec::new(),
        secret_writes: Vec::new(),
        connection_field_secrets: Vec::new(),
        kind_failures: Vec::new(),
        unresolved_ref_connections: Vec::new(),
        value_ref_failures: Vec::new(),
        chosen_auth_profiles: Vec::new(),
    };

    let outcome = datatab_app::portability::persist_import_actions(
        actions,
        &mut AppStatePersistence::new(&mut state),
    );

    assert_eq!(outcome.succeeded, ["literal password"]);
    assert_eq!(
        keyring.entry(&password_ref).as_deref(),
        Some(PASSWORD_SENTINEL)
    );
    assert!(state.profiles()[0].value_refs.is_empty());
    let database = database_bytes(&state);
    assert!(contains(&database, "db.example"), "the database was read");
    assert!(!contains(&database, PASSWORD_SENTINEL));
}

mod import_async;
