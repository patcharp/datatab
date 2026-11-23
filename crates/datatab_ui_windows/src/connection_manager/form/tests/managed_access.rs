use super::*;
use crate::connection_manager::AccessTabMode;
use datatab_core::AccessKind;

fn stored_access_kind(state: &AppStateEntity) -> serde_json::Value {
    let profiles = state.profiles();
    assert_eq!(profiles.len(), 1);
    serde_json::to_value(&profiles[0].access_kind).expect("serialize access kind")
}

/// The form has no Managed access method, so a stored Managed profile opens
/// as Direct. A save that does not touch the access method must keep the
/// stored access kind, with every parameter (#350, Lead ruling 1 item 6).
#[::core::prelude::v1::test]
fn stored_managed_profile_stays_managed_after_untouched_save() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let runtime = StorageRuntime::in_memory().expect("storage");
    let mut profile = sqlite_profile("managed");
    profile.access_kind = Some(AccessKind::Managed {
        provider: "custom-provider".into(),
        params: HashMap::from([
            ("target".to_string(), "db-1".to_string()),
            ("extra".to_string(), "kept".to_string()),
        ]),
    });
    let expected = serde_json::to_value(&profile.access_kind).expect("serialize access kind");
    datatab_app::config_loader::save_profiles(
        &runtime,
        std::slice::from_ref(&profile),
        &std::collections::HashSet::new(),
    )
    .expect("save profile");
    let state = cx.update(|cx| {
        cx.new(|_| AppStateEntity::new_with_storage_runtime(runtime).expect("load state"))
    });
    state.update(&mut cx, |state, _| {
        *state.secret_store().write().expect("secret store") =
            Box::new(SecretStoreFixture::new(PasswordSaveOutcome::Success));
    });
    let profile = state.read_with(&cx, |state, _| {
        assert_eq!(stored_access_kind(state), expected, "the profile loads");
        state.profiles()[0].clone()
    });

    let window = open_edit_profile_window(state.clone(), profile, &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            assert_eq!(manager.access.access_tab_mode, AccessTabMode::Direct);
            manager.save_profile(window, cx)
        })
        .expect("save untouched form");
    cx.run_until_parked();
    assert!(window.root(&mut cx).is_err(), "saved form closes");

    state.read_with(&cx, |state, _| {
        assert_eq!(state.profiles()[0].name, "edited profile");
        assert_eq!(stored_access_kind(state), expected);

        let reloaded = AppStateEntity::new_with_storage_runtime(
            StorageRuntime::for_path(state.storage_runtime().datatab_db_path().to_path_buf())
                .expect("reopen saved database"),
        )
        .expect("reload");
        assert_eq!(stored_access_kind(&reloaded), expected);
    });
}
