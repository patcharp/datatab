use super::*;

const SENTINEL: &str = "sentinel-api-token-7f3a91";
const DRIVER_ID: &str = "rpc:fake";

static FORM: std::sync::LazyLock<datatab_core::DriverFormDef> = std::sync::LazyLock::new(|| {
    use datatab_core::{FormFieldKind, FormSection, FormTab, field};
    let mut api_token = field("api_token", "API token", FormFieldKind::Password, "");
    api_token.required = true;
    datatab_core::DriverFormDef {
        tabs: vec![FormTab {
            id: "main".into(),
            label: "Main".into(),
            sections: vec![FormSection {
                title: "Auth".into(),
                icon: None,
                fields: vec![field("host", "Host", FormFieldKind::Text, ""), api_token],
            }],
        }],
    }
});

struct FakeRpcDriver(datatab_core::DriverMetadata);

impl datatab_core::DbDriver for FakeRpcDriver {
    fn kind(&self) -> datatab_core::DbKind {
        datatab_core::DbKind::SQLite
    }
    fn metadata(&self) -> &datatab_core::DriverMetadata {
        &self.0
    }
    fn form_definition(&self) -> &datatab_core::DriverFormDef {
        &FORM
    }
    fn driver_key(&self) -> datatab_core::DriverKey {
        DRIVER_ID.into()
    }
    fn build_config(
        &self,
        values: &datatab_core::FormValues,
    ) -> Result<DbConfig, datatab_core::DbError> {
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
    fn requires_password(&self) -> bool {
        false
    }
    fn connect_with_secrets(
        &self,
        _: &ConnectionProfile,
        _: Option<&SecretString>,
        _: Option<&SecretString>,
    ) -> Result<Box<dyn datatab_core::Connection>, datatab_core::DbError> {
        Err(datatab_core::DbError::NotSupported("test driver".into()))
    }
    fn test_connection(&self, _: &ConnectionProfile) -> Result<(), datatab_core::DbError> {
        Ok(())
    }
}

pub(super) fn register_rpc_driver(app_state: &Entity<AppStateEntity>, cx: &mut TestAppContext) {
    let metadata = datatab_core::DriverMetadataBuilder::new(
        "fake",
        "Fake RPC",
        datatab_core::DatabaseCategory::Relational,
        datatab_core::QueryLanguage::Sql,
    )
    .build();
    app_state.update(cx, |state, _| {
        state.register_driver_for_test(DRIVER_ID, Arc::new(FakeRpcDriver(metadata)))
    });
}

pub(super) fn rpc_profile() -> ConnectionProfile {
    ConnectionProfile::new_with_driver(
        "rpc profile",
        datatab_core::DbKind::SQLite,
        DRIVER_ID,
        DbConfig::External {
            kind: datatab_core::DbKind::SQLite,
            values: HashMap::from([("host".to_string(), "rpc.example".to_string())]),
        },
    )
}

/// Editing a profile whose secret field lives in the keyring, with that
/// required field left empty, saves and keeps the saved value.
#[::core::prelude::v1::test]
fn an_edit_save_with_the_secret_field_empty_keeps_the_saved_value() {
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let app_state = test_app_state(&mut cx, fixture.clone());
    register_rpc_driver(&app_state, &mut cx);
    let mut profile = rpc_profile();
    profile.keyring_secret_fields.insert("api_token".into());
    let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");
    fixture.seed_ref(&secret_ref, SENTINEL);
    app_state.update(&mut cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None)
    });

    let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            manager.form.input_name.update(cx, |input, cx| {
                input.set_value("renamed rpc profile", window, cx)
            });
            manager.save_profile(window, cx);
        })
        .expect("edit window stays available");
    cx.run_until_parked();

    let saved = cx.update(|cx| app_state.read(cx).profiles().to_vec());
    assert_eq!(
        saved[0].name, "renamed rpc profile",
        "the edit is committed"
    );
    assert!(saved[0].keyring_secret_fields.contains("api_token"));
    let values = match &saved[0].config {
        DbConfig::External { values, .. } => values.clone(),
        other => panic!("expected an external config, got {other:?}"),
    };
    assert!(!values.contains_key("api_token"));
    let kept = fixture
        .values
        .lock()
        .expect("test secret store value lock poisoned")
        .get(&secret_ref)
        .map(|secret| secret.expose_secret().to_string());
    assert_eq!(kept.as_deref(), Some(SENTINEL));
}

/// A typed secret field the keyring refuses stops the save before anything
/// is written, and the form names the field.
#[::core::prelude::v1::test]
fn a_keyring_that_refuses_a_typed_secret_field_refuses_the_save() {
    let mut cx = TestAppContext::single();
    let host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::FailBeforeWrite);
    let app_state = test_app_state(&mut cx, fixture.clone());
    register_rpc_driver(&app_state, &mut cx);
    let window = cx
        .update(|cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| ConnectionManagerWindow::new(app_state.clone(), window, cx))
            })
        })
        .expect("connection manager window opens");

    window
        .update(&mut cx, |manager, window, cx| {
            manager.select_driver(DRIVER_ID, window, cx);
            manager.form.input_name.update(cx, |input, cx| {
                input.set_value("new rpc profile", window, cx)
            });
            for (field_id, value) in [("host", "rpc.example"), ("api_token", SENTINEL)] {
                manager
                    .form
                    .driver_inputs
                    .get(field_id)
                    .expect("driver input")
                    .update(cx, |input, cx| input.set_value(value, window, cx));
            }
            manager.settings_tab.conn_override_refresh_interval = true;
            manager
                .settings_tab
                .conn_refresh_interval_input
                .update(cx, |input, cx| input.set_value("invalid", window, cx));
            manager.save_profile(window, cx);
            assert_eq!(
                host.read(cx).toast_count(),
                1,
                "settings warning appears before the secret-field write completes"
            );
        })
        .expect("a refused save leaves the form window available");
    cx.run_until_parked();
    let reason_shown = window
        .update(&mut cx, |manager, _, _| {
            manager.validation_errors.contains(&datatab_i18n::t!(
                "form.validation.secret_field_keyring_failed",
                field = "API token"
            ))
        })
        .expect("a refused save leaves the form window available");

    assert!(reason_shown, "the form names the field");
    assert_eq!(
        host.read_with(&cx, |host, _| host.toast_count()),
        1,
        "settings warning remains when a secret-field write is refused"
    );
    assert!(
        cx.update(|cx| app_state.read(cx).profiles().is_empty()),
        "nothing is committed"
    );
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close the test window");
}

/// An edit whose secret field was stored but whose primary password write
/// then fails is refused, and the field's keyring entry is put back as it
/// was (#411 triage round 2, point 1).
#[::core::prelude::v1::test]
fn a_save_refused_by_the_password_write_restores_the_secret_field() {
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let app_state = test_app_state(&mut cx, fixture.clone());
    register_rpc_driver(&app_state, &mut cx);
    let mut profile = rpc_profile();
    profile.keyring_secret_fields.insert("api_token".into());
    let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");
    fixture.seed_ref(&secret_ref, SENTINEL);
    fixture.fail_writes_of(profile.secret_ref());
    app_state.update(&mut cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None)
    });

    let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            manager
                .form
                .driver_inputs
                .get("api_token")
                .expect("driver input")
                .update(cx, |input, cx| input.set_value("new-token", window, cx));
            manager.form.form_save_password = true;
            manager
                .form
                .input_password
                .update(cx, |input, cx| input.set_value("primary", window, cx));
            manager.save_profile(window, cx);
        })
        .expect("a refused save leaves the form window available");
    cx.run_until_parked();

    let kept = fixture
        .values
        .lock()
        .expect("test secret store value lock poisoned")
        .get(&secret_ref)
        .map(|secret| secret.expose_secret().to_string());
    assert_eq!(kept.as_deref(), Some(SENTINEL));
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close the test window");
}

fn stored_value_ref_state(
    app_state: &Entity<AppStateEntity>,
    profile_id: uuid::Uuid,
    cx: &mut TestAppContext,
) -> (Option<String>, Option<String>) {
    app_state.read_with(cx, |state, _| {
        let rows = state
            .storage_runtime()
            .connection_profiles()
            .value_refs()
            .get_for_profile(&profile_id.to_string())
            .expect("rows");
        let env = rows
            .iter()
            .find(|row| row.ref_key == "api_token")
            .map(|row| format!("{row:?}"));
        let saved = state.profiles()[0].config.clone();
        let token = match saved {
            DbConfig::External { values, .. } => values.get("api_token").cloned(),
            _ => None,
        };
        (env, token)
    })
}

fn rpc_profile_with_env_ref() -> ConnectionProfile {
    let mut profile = rpc_profile();
    profile
        .value_refs
        .insert("api_token".into(), ValueRef::env("API_TOKEN"));
    profile
}

/// The shape of #414: an imported `Env` ref on an RPC driver's secret field,
/// which has no value-source selector. A rename-and-save keeps it.
#[::core::prelude::v1::test]
fn an_edit_save_keeps_the_imported_env_ref_of_a_secret_field_without_selector() {
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let app_state = test_app_state(&mut cx, fixture.clone());
    register_rpc_driver(&app_state, &mut cx);
    let profile = rpc_profile_with_env_ref();
    app_state.update(&mut cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None)
    });

    let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            manager
                .form
                .input_name
                .update(cx, |input, cx| input.set_value("renamed", window, cx));
            manager.save_profile(window, cx);
        })
        .expect("edit window stays available");
    cx.run_until_parked();

    let (env, token) = stored_value_ref_state(&app_state, profile.id, &mut cx);
    assert_eq!(
        app_state.read_with(&cx, |s, _| s.profiles()[0].name.clone()),
        "renamed"
    );
    assert!(env.is_some_and(|row| row.contains("API_TOKEN")), "ref kept");
    assert_eq!(token, None);
}

/// A token typed over the field drops the old ref and goes to the keyring,
/// never the database.
#[::core::prelude::v1::test]
fn a_typed_secret_field_replaces_its_stored_value_ref() {
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let app_state = test_app_state(&mut cx, fixture.clone());
    register_rpc_driver(&app_state, &mut cx);
    let profile = rpc_profile_with_env_ref();
    app_state.update(&mut cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None)
    });

    let window = open_untouched_edit_window(app_state.clone(), profile.clone(), &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            manager
                .form
                .driver_inputs
                .get("api_token")
                .expect("driver input")
                .update(cx, |input, cx| input.set_value(SENTINEL, window, cx));
            manager.save_profile(window, cx);
        })
        .expect("edit window stays available");
    cx.run_until_parked();

    let (env, token) = stored_value_ref_state(&app_state, profile.id, &mut cx);
    assert_eq!(env, None, "the typed value replaces the ref");
    assert_eq!(token, None, "the token is not in the database config");
    let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");
    let stored = fixture
        .values
        .lock()
        .expect("test secret store value lock poisoned")
        .get(&secret_ref)
        .map(|secret| secret.expose_secret().to_string());
    assert_eq!(stored.as_deref(), Some(SENTINEL));
}
