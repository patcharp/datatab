use super::*;
use crate::connection_manager::import_panel::AppStatePersistence;
use datatab_app::portability::{ImportOutcome, apply_import};
use datatab_core::{AuthProfile, DriverFormDef, FormFieldKind, FormSection, FormTab};
use datatab_portability::bundle::{
    Bundle, BundleMeta, CURRENT_FORMAT_VERSION, ConnectionEntry, EncryptionMode, RequiredRef,
    RequiredRefKind,
};
use datatab_portability::{DestSnapshot, ParsedBundle, ResolutionChoices};

const AUTH_DRIVER_ID: &str = "rpc:auth.sock";

/// An RPC-style driver whose form declares an `AuthProfileRef` field `profile`.
struct AuthRefDriver {
    inner: Arc<dyn DbDriver>,
    form: DriverFormDef,
}

impl AuthRefDriver {
    fn new(inner: Arc<dyn DbDriver>) -> Self {
        let mut field = inner
            .form_definition()
            .tabs
            .iter()
            .flat_map(|tab| &tab.sections)
            .flat_map(|section| &section.fields)
            .next()
            .cloned()
            .expect("the SQLite form has a field to copy");
        field.id = "profile".to_string();
        field.kind = FormFieldKind::AuthProfileRef { provider_id: None };
        let form = DriverFormDef {
            tabs: vec![FormTab {
                id: "main".to_string(),
                label: "Main".to_string(),
                sections: vec![FormSection {
                    title: "Auth".to_string(),
                    icon: None,
                    fields: vec![field],
                }],
            }],
        };
        Self { inner, form }
    }
}

impl DbDriver for AuthRefDriver {
    fn kind(&self) -> datatab_core::DbKind {
        self.inner.kind()
    }
    fn metadata(&self) -> &datatab_core::DriverMetadata {
        self.inner.metadata()
    }
    fn form_definition(&self) -> &DriverFormDef {
        &self.form
    }
    fn driver_key(&self) -> datatab_core::DriverKey {
        self.inner.driver_key()
    }
    fn build_config(
        &self,
        values: &datatab_core::FormValues,
    ) -> Result<DbConfig, datatab_core::DbError> {
        Ok(DbConfig::External {
            kind: self.inner.kind(),
            values: values.clone(),
        })
    }
    fn extract_values(&self, config: &DbConfig) -> datatab_core::FormValues {
        match config {
            DbConfig::External { values, .. } => values.clone(),
            _ => Default::default(),
        }
    }
    fn connect_with_secrets(
        &self,
        profile: &ConnectionProfile,
        password: Option<&SecretString>,
        ssh_secret: Option<&SecretString>,
    ) -> Result<Box<dyn datatab_core::Connection>, datatab_core::DbError> {
        self.inner
            .connect_with_secrets(profile, password, ssh_secret)
    }
    fn test_connection(&self, profile: &ConnectionProfile) -> Result<(), datatab_core::DbError> {
        self.inner.test_connection(profile)
    }
}

fn bundle_needing_an_auth_profile() -> ParsedBundle {
    let entry = ConnectionEntry {
        local_id: "needs-auth".to_string(),
        name: "needs-auth".to_string(),
        driver_id: AUTH_DRIVER_ID.to_string(),
        fields: Default::default(),
        local_path_fields: Default::default(),
        required_refs: vec![RequiredRef {
            field: "profile".to_string(),
            kind: RequiredRefKind::AuthProfile,
        }],
        auth_profile_local_id: None,
        access: None,
        value_refs: Default::default(),
        include_hooks: false,
        include_settings_overrides: false,
        hooks_payload: None,
        settings_overrides_payload: None,
        kind: Some("SQLite".to_string()),
        uri_secret_fields: vec![],
    };
    ParsedBundle {
        bundle: Bundle {
            bundle: BundleMeta {
                format_version: CURRENT_FORMAT_VERSION,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                datatab_version: "0.7.0-dev.0".to_string(),
                encryption: EncryptionMode::None,
            },
            drivers: vec![],
            connections: vec![entry],
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            secrets: None,
        },
        decrypted_secrets: Some(HashMap::from([(
            "conn:needs-auth:password".to_string(),
            "bundle-password".to_string(),
        )])),
    }
}

fn import_with_choice(
    state: &mut datatab_app::AppState,
    chosen: Option<uuid::Uuid>,
) -> ImportOutcome {
    let inner = state
        .drivers()
        .get("sqlite")
        .cloned()
        .expect("SQLite driver registered");
    state.register_driver_for_test(AUTH_DRIVER_ID, Arc::new(AuthRefDriver::new(inner)));

    let parsed = bundle_needing_an_auth_profile();
    let plan = datatab_portability::import::plan(
        &parsed,
        &DestSnapshot {
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            connections: vec![],
        },
    );
    let mut choices = ResolutionChoices::default();
    if let Some(id) = chosen {
        choices
            .auth_profile_choices
            .insert(("needs-auth".to_string(), "profile".to_string()), id);
    }
    let mut persistence = AppStatePersistence::new(state);
    apply_import(&parsed, &plan, &choices, &mut persistence).expect("import")
}

#[::core::prelude::v1::test]
fn chosen_auth_profile_is_saved_with_its_form_field_and_reloads() {
    let mut cx = TestAppContext::single();
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let state = test_app_state(&mut cx, fixture.clone());

    state.update(&mut cx, |state, _| {
        let mut auth = AuthProfile::new("Dest SSO", "custom", HashMap::new());
        auth.fields
            .insert("profile_name".to_string(), "sso-prod".to_string());
        let auth_id = auth.id;
        state.add_auth_profile(auth);

        let outcome = import_with_choice(state, Some(auth_id));
        assert!(outcome.config_failures.is_empty(), "{outcome:?}");

        let saved = state.profiles().first().cloned().expect("saved connection");
        assert_eq!(saved.auth_profile_id, Some(auth_id));
        let DbConfig::External { values, .. } = &saved.config else {
            panic!("an RPC driver profile is External");
        };
        assert_eq!(values.get("profile").map(String::as_str), Some("sso-prod"));

        let reopened =
            StorageRuntime::for_path(state.storage_runtime().datatab_db_path().to_path_buf())
                .expect("reopen store");
        let reloaded =
            datatab_app::AppState::new_with_storage_runtime(reopened).expect("reload state");
        let again = reloaded.profiles().first().cloned().expect("reloaded");
        assert_eq!(again.auth_profile_id, Some(auth_id));
        let DbConfig::External { values, .. } = &again.config else {
            panic!("reloaded profile is External");
        };
        assert_eq!(values.get("profile").map(String::as_str), Some("sso-prod"));
    });
}

#[::core::prelude::v1::test]
fn a_choice_naming_a_missing_profile_writes_nothing() {
    let mut cx = TestAppContext::single();
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let state = test_app_state(&mut cx, fixture.clone());

    state.update(&mut cx, |state, _| {
        let missing = uuid::Uuid::new_v4();
        let outcome = import_with_choice(state, Some(missing));

        assert_eq!(outcome.config_failures.len(), 1, "{outcome:?}");
        assert!(outcome.config_failures[0].1.contains(&missing.to_string()));
        assert!(state.profiles().is_empty());
        assert!(state.connection_tree().nodes.is_empty(), "no folder entry");
    });
    assert!(
        fixture
            .values
            .lock()
            .expect("test secret store value lock poisoned")
            .is_empty(),
        "no secret is written"
    );
}

#[::core::prelude::v1::test]
fn no_choice_imports_the_connection_as_before() {
    let mut cx = TestAppContext::single();
    let state = test_app_state(
        &mut cx,
        SecretStoreFixture::new(PasswordSaveOutcome::Success),
    );

    state.update(&mut cx, |state, _| {
        let outcome = import_with_choice(state, None);
        assert!(outcome.config_failures.is_empty(), "{outcome:?}");
        let saved = state.profiles().first().cloned().expect("saved connection");
        assert_eq!(saved.auth_profile_id, None);
        let DbConfig::External { values, .. } = &saved.config else {
            panic!("External");
        };
        assert!(!values.contains_key("profile"));
    });
}
