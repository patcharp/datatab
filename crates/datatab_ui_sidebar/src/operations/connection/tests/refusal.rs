use super::*;

#[gpui::test]
fn refused_profile_fails_before_password_prompt(cx: &mut TestAppContext) {
    let (fixture, cx) = password_fixture_with_refusal(false, true, cx);
    let profile_id = fixture.profile_id;

    fixture
        .sidebar
        .update(cx, |sidebar, cx| sidebar.connect_to_profile(profile_id, cx));
    cx.run_until_parked();

    assert!(fixture.prompts.lock().expect("prompts").is_empty());
    assert_eq!(toast_stack(cx).0, 1, "one refusal is reported");
    assert!(
        fixture
            .sidebar
            .read_with(cx, |sidebar, _| sidebar.pending_toast.is_none()),
        "no duplicate pending toast"
    );
    assert!(recorded(&fixture.passwords).is_empty());
    fixture.state.read_with(cx, |state, _| {
        assert_eq!(
            state.unread_error_count, 0,
            "driver refusals keep develop handling without audit reporting"
        );
        let error = state.connect_failure(profile_id).expect("connect failure");
        assert!(error.contains("fixture refusal"), "{error}");
        assert!(!state.connections().contains_key(&profile_id));
        assert!(!state.is_operation_pending(profile_id, None));
    });
}

struct PanicOnReadSecretStore;

impl datatab_core::SecretStore for PanicOnReadSecretStore {
    fn is_available(&self) -> bool {
        true
    }
    fn get(&self, _: &str) -> Result<Option<SecretString>, datatab_core::DbError> {
        panic!("flagged profile read the secret store")
    }
    fn set(&self, _: &str, _: &SecretString) -> Result<(), datatab_core::DbError> {
        panic!("flagged profile wrote the secret store")
    }
    fn delete(&self, _: &str) -> Result<(), datatab_core::DbError> {
        panic!("flagged profile deleted a secret")
    }
}

fn flag_profile(fixture: &PasswordFixture, cx: &mut VisualTestContext) {
    fixture.state.update(cx, |state, _| {
        let mut profile = state.profiles()[0].clone();
        profile.value_refs.insert("password".into(), datatab_core::ValueRef::dotenv("unused", "PASSWORD"));
        profile.value_refs.insert("host".into(), datatab_core::ValueRef::secret("unused", "unused", None));
        let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
        datatab_app::config_loader::save_profiles(&runtime, &[profile], &std::collections::HashSet::new()).expect("save");
        runtime.open_datatab_db().expect("db").execute(
            "UPDATE cfg_connection_profile_value_refs SET env_key = NULL, ref_provider = NULL WHERE profile_id = ?1",
            [fixture.profile_id.to_string()],
        ).expect("break references");
        state.inner = datatab_app::AppState::new_with_storage_runtime(runtime).expect("load");
        *state.secret_store().write().expect("store") = Box::new(PanicOnReadSecretStore);
    });
}

#[gpui::test]
fn broken_value_ref_duplicate_refuses_without_reading_or_creating(cx: &mut TestAppContext) {
    let (fixture, cx) = password_fixture(true, cx);
    flag_profile(&fixture, cx);
    let item_id = datatab_core::SchemaNodeId::Profile {
        profile_id: fixture.profile_id,
    }
    .to_string();
    fixture
        .sidebar
        .update(cx, |sidebar, cx| sidebar.duplicate_profile(&item_id, cx));
    cx.run_until_parked();
    let (count, title) = toast_stack(cx);
    assert_eq!(count, 1);
    let title = title.expect("refusal message");
    assert!(
        title.contains("password") && title.contains("host"),
        "{title}"
    );
    fixture.state.read_with(cx, |state, _| {
        assert_eq!(state.profiles().len(), 1);
        assert_eq!(state.unread_error_count, 1, "one report_error");
    });
}

#[gpui::test]
fn broken_value_ref_connect_reports_once(cx: &mut TestAppContext) {
    let (fixture, cx) = password_fixture(true, cx);
    flag_profile(&fixture, cx);
    fixture.sidebar.update(cx, |sidebar, cx| {
        sidebar.connect_to_profile(fixture.profile_id, cx)
    });
    cx.run_until_parked();
    assert_eq!(toast_stack(cx).0, 1);
    fixture
        .state
        .read_with(cx, |state, _| assert_eq!(state.unread_error_count, 1));
    assert!(fixture.prompts.lock().expect("prompts").is_empty());
}

#[gpui::test]
fn broken_value_ref_change_keeps_rpc_refusal_out_of_error_reporter(cx: &mut TestAppContext) {
    let (fixture, cx) = password_fixture(true, cx);
    fixture.state.update(cx, |state, _| {
        state.profiles_mut()[0].driver_id = Some("rpc:missing-fixture".into());
    });
    fixture.sidebar.update(cx, |sidebar, cx| {
        sidebar.connect_to_profile(fixture.profile_id, cx)
    });
    fixture.state.read_with(cx, |state, _| {
        assert_eq!(
            state.unread_error_count, 0,
            "RPC refusal must keep develop handling"
        )
    });
    cx.run_until_parked();
    assert_eq!(toast_stack(cx).0, 1, "develop shows the queued refusal");
}
