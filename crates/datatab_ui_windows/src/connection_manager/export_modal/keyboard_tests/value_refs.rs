use super::*;

struct PanicOnReadSecretStore;
impl datatab_core::SecretStore for PanicOnReadSecretStore {
    fn is_available(&self) -> bool {
        true
    }
    fn get(
        &self,
        _: &str,
    ) -> Result<Option<datatab_core::secrecy::SecretString>, datatab_core::DbError> {
        panic!("flagged export read a secret")
    }
    fn set(
        &self,
        _: &str,
        _: &datatab_core::secrecy::SecretString,
    ) -> Result<(), datatab_core::DbError> {
        panic!("flagged export wrote a secret")
    }
    fn delete(&self, _: &str) -> Result<(), datatab_core::DbError> {
        panic!("flagged export deleted a secret")
    }
}

#[gpui::test]
fn broken_value_ref_export_refuses_before_reading_or_writing(cx: &mut TestAppContext) {
    let (modal, _, cx, _) = open_modal(cx);
    let runtime = StorageRuntime::in_memory().expect("storage");
    let mut profile = datatab_core::ConnectionProfile::new(
        "broken export",
        datatab_core::DbConfig::default_sqlite(),
    );
    profile.save_password = true;
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::dotenv("unused", "PASSWORD"),
    );
    datatab_app::config_loader::save_profiles(
        &runtime,
        std::slice::from_ref(&profile),
        &std::collections::HashSet::new(),
    )
    .expect("save");
    runtime
        .open_datatab_db()
        .expect("db")
        .execute(
            "UPDATE cfg_connection_profile_value_refs SET env_key = NULL",
            [],
        )
        .expect("break row");
    let output = runtime
        .datatab_db_path()
        .with_file_name("refused-export.toml");
    cx.update(|window, cx| {
        modal.update(cx, |modal, cx| {
            modal.app_state.update(cx, |state, _| {
                state.inner =
                    datatab_app::AppState::new_with_storage_runtime(runtime).expect("reload");
                *state.secret_store().write().expect("store") = Box::new(PanicOnReadSecretStore);
            });
            modal.target = Some(ExportTarget::Connection(profile.id));
            modal.output_input.update(cx, |input, cx| {
                input.set_value(output.to_string_lossy().to_string(), window, cx)
            });
            modal.do_export(window, cx);
            assert!(!modal.is_exporting, "flagged export must not start");
            let error = modal.validation_error.as_deref().expect("refusal banner");
            assert!(
                error.contains("password") && error.contains("key"),
                "{error}"
            );
        });
    });
    cx.run_until_parked();
    assert!(!output.exists(), "refused export creates no bundle");
}
