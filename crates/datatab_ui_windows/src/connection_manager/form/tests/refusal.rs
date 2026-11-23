use super::*;

#[::core::prelude::v1::test]
fn test_connection_refuses_direct_profile_during_preparation() {
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let state = test_app_state(
        &mut cx,
        SecretStoreFixture::new(PasswordSaveOutcome::Success),
    );
    let window = open_new_profile_window(state, &mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            let driver = manager
                .form
                .selected_driver
                .clone()
                .expect("selected driver");
            manager.form.selected_driver = Some(Arc::new(RejectingDriver {
                inner: driver,
                refuses_profile: true,
                declared_kind: None,
            }));
            assert!(
                !manager
                    .build_profile(cx)
                    .expect("test profile")
                    .uses_pipeline()
            );
            manager.test_connection(window, cx);
            assert!(matches!(manager.test_status, TestStatus::Failed));
            assert!(
                manager
                    .test_error
                    .as_deref()
                    .is_some_and(|error| error.contains("fixture refusal"))
            );
        })
        .expect("test window open");
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close test window");
}

/// An RPC driver can still declare a reserved driver-wire kind in its Hello
/// (#364). Its profile could not be loaded again, so the save is refused in
/// the form with the reason: nothing is committed and no secret is written.
#[::core::prelude::v1::test]
fn save_refuses_a_driver_that_declares_a_reserved_kind() {
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let state = test_app_state(&mut cx, fixture.clone());
    let window = open_new_profile_window(state.clone(), &mut cx);

    let reason_shown = window
        .update(&mut cx, |manager, window, cx| {
            let driver = manager
                .form
                .selected_driver
                .clone()
                .expect("selected driver");
            manager.form.selected_driver = Some(Arc::new(RejectingDriver {
                inner: driver,
                refuses_profile: false,
                declared_kind: Some(datatab_core::DbKind::ReservedDynamoDb),
            }));
            manager.save_profile(window, cx);
            manager
                .validation_errors
                .contains(&datatab_i18n::t!("form.validation.reserved_database_kind"))
        })
        .expect("a refused save leaves the form window available");

    assert!(reason_shown, "the form names why the save was refused");
    assert!(window.root(&mut cx).is_ok(), "the form window remains open");
    assert!(
        cx.update(|cx| state.read(cx).profiles().is_empty()),
        "a profile with a reserved kind is not committed"
    );
    assert!(
        fixture
            .values
            .lock()
            .expect("test secret store value lock poisoned")
            .is_empty(),
        "no secret is written for a refused profile"
    );

    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close the test window");
}

/// A bundle connection is rebuilt with its registered driver. When that
/// driver declares a reserved kind, the rebuilt profile is refused with the
/// reason, like any other config failure: it never enters the profile list.
#[::core::prelude::v1::test]
fn import_refuses_a_connection_rebuilt_with_a_reserved_kind() {
    use crate::connection_manager::import_panel::AppStatePersistence;
    use datatab_app::portability::{ConnectionInsertResult, ImportPersistence};

    let mut cx = TestAppContext::single();
    let state = test_app_state(
        &mut cx,
        SecretStoreFixture::new(PasswordSaveOutcome::Success),
    );

    state.update(&mut cx, |state, _| {
        let inner = state
            .drivers()
            .get("sqlite")
            .cloned()
            .expect("SQLite driver registered");
        state.register_driver_for_test(
            "rpc:reserved.sock",
            Arc::new(RejectingDriver {
                inner,
                refuses_profile: false,
                declared_kind: Some(datatab_core::DbKind::ReservedDynamoDb),
            }),
        );
        // The bundle names a kept kind; only the driver's own config is reserved.
        let imported = ConnectionProfile::new_with_driver(
            "imported",
            datatab_core::DbKind::SQLite,
            "rpc:reserved.sock",
            DbConfig::External {
                kind: datatab_core::DbKind::SQLite,
                values: datatab_core::FormValues::new(),
            },
        );

        match AppStatePersistence::new(state).add_connection(imported) {
            ConnectionInsertResult::ConfigFailed(reason) => {
                assert!(reason.contains("reserved database kind"), "{reason}");
            }
            ConnectionInsertResult::Ok => panic!("a reserved kind was imported"),
            ConnectionInsertResult::NeedsDriver => panic!("the test driver is registered"),
        }
        assert!(state.profiles().is_empty());
    });
}

#[::core::prelude::v1::test]
fn unknown_form_literal_refusal_names_the_cause_before_secret_writes() {
    let mut cx = TestAppContext::single();
    let host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let state = test_app_state(&mut cx, fixture.clone());
    let mut profile = sqlite_profile("unknown form");
    profile
        .value_refs
        .insert("extra_field".into(), ValueRef::literal("literal-436"));
    state.update(&mut cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None);
        // Model a newly supplied ref that has no value-source selector today.
        state
            .storage_runtime()
            .connection_profiles()
            .value_refs()
            .delete_for_profile(&profile.id.to_string())
            .expect("remove stored ref");
    });
    let window = open_new_profile_window(state.clone(), &mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            manager.editing_profile_id = Some(profile.id);
            let inner = manager
                .form
                .selected_driver
                .clone()
                .expect("selected driver");
            manager.form.selected_driver = Some(Arc::new(RejectingDriver {
                inner,
                refuses_profile: false,
                declared_kind: Some(datatab_core::DbKind::SQLite),
            }));
            manager.form.selected_driver_id = Some("unregistered".into());
            let built = manager.build_profile(cx).expect("profile");
            assert!(built.value_refs.contains_key("extra_field"));
            manager.save_profile(window, cx);
            assert!(
                manager.validation_errors.contains(&datatab_i18n::t!(
                    "form.validation.unknown_form_literal_ref"
                )),
                "show the unknown-form cause: {:?}",
                manager.validation_errors
            );
            assert!(!manager.validation_errors.contains(&datatab_i18n::t!(
                "form.validation.broken_password_unresolved"
            )));
            window.remove_window();
        })
        .expect("refused form");
    assert_eq!(host.read_with(&cx, |host, _| host.toast_count()), 1);
    assert_eq!(
        state.read_with(&cx, |state, _| state.profiles()[0].name.clone()),
        profile.name
    );
    assert!(fixture.values.lock().expect("keyring").is_empty());
}

#[::core::prelude::v1::test]
fn unknown_form_storage_read_failure_reports_storage_once_before_secret_writes() {
    let mut cx = TestAppContext::single();
    let host = init_form_test_runtime(&mut cx);
    let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
    let state = test_app_state(&mut cx, fixture.clone());
    let window = open_new_profile_window(state.clone(), &mut cx);
    state.read_with(&cx, |state, _| {
        state
            .storage_runtime()
            .open_datatab_db()
            .expect("database")
            .execute("DROP TABLE cfg_connection_profile_value_refs", [])
            .expect("make the guard read fail");
    });

    window
        .update(&mut cx, |manager, window, cx| {
            let inner = manager
                .form
                .selected_driver
                .clone()
                .expect("selected driver");
            manager.form.selected_driver = Some(Arc::new(RejectingDriver {
                inner,
                refuses_profile: false,
                declared_kind: Some(datatab_core::DbKind::SQLite),
            }));
            manager.form.selected_driver_id = Some("unregistered".into());
            let built = manager.build_profile(cx).expect("profile");
            assert!(matches!(
                state.read(cx).check_connection_manager_save(&built, ""),
                Err(datatab_app::app_state::ProfileSaveError::Storage(_))
            ));
            manager.save_profile(window, cx);
            assert_eq!(
                manager.validation_errors,
                vec![datatab_i18n::t!(
                    "form.validation.profile_storage_read_failed"
                )]
            );
            window.remove_window();
        })
        .expect("refused form");

    assert_eq!(host.read_with(&cx, |host, _| host.toast_count()), 1);
    assert_eq!(
        host.read_with(&cx, |host, _| host.last_toast_occurrences()),
        Some(1)
    );
    state.read_with(&cx, |state, _| {
        assert!(state.profiles().is_empty());
        assert!(
            state
                .storage_runtime()
                .connection_profiles()
                .all()
                .expect("profiles")
                .is_empty()
        );
    });
    assert!(fixture.values.lock().expect("keyring").is_empty());
}
