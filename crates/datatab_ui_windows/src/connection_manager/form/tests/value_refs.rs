use super::*;
use uuid::Uuid;

#[::core::prelude::v1::test]
fn broken_value_ref_blocks_test_until_connection_manager_save() {
    let mut cx = TestAppContext::single();
    let host = init_form_test_runtime(&mut cx);
    let runtime = StorageRuntime::in_memory().expect("storage");
    let mut profile = sqlite_profile("broken source");
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::dotenv("unused-source", "PASSWORD"),
    );
    datatab_app::config_loader::save_profiles(&runtime, std::slice::from_ref(&profile))
        .expect("save profile");
    runtime
        .open_datatab_db()
        .expect("database")
        .execute(
            "UPDATE cfg_connection_profile_value_refs SET env_key = NULL WHERE profile_id = ?1",
            [profile.id.to_string()],
        )
        .expect("break reference");
    let profile_id = profile.id;
    let state = cx.update(|cx| {
        cx.new(|_| AppStateEntity::new_with_storage_runtime(runtime).expect("load state"))
    });
    state.update(&mut cx, |state, _| {
        *state.secret_store().write().expect("secret store") =
            Box::new(SecretStoreFixture::new(PasswordSaveOutcome::Success));
    });
    let profile = state.read_with(&cx, |state, _| state.profiles()[0].clone());
    let window = open_edit_profile_window(state.clone(), profile, &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            manager.test_connection(window, cx);
            assert!(matches!(manager.test_status, TestStatus::Failed));
            assert_eq!(
                manager.test_error.as_deref(),
                Some("Dotenv value ref 'password' is missing its key\nChoose a value source for each broken field, then save first. This connection cannot connect until you save.")
            );
            assert!(
                !manager
                    .app_state
                    .read(cx)
                    .value_ref_diagnostics(profile_id)
                    .is_empty()
            );
        })
        .expect("test refused");
    assert_eq!(cx.update(|cx| host.read(cx).toast_count()), 0);
    window
        .update(&mut cx, |manager, window, cx| {
            manager.save_profile(window, cx)
        })
        .expect("save repaired form");
    cx.run_until_parked();
    state.read_with(&cx, |state, _| {
        assert!(state.value_ref_diagnostics(profile_id).is_empty());
        assert_eq!(state.validate_connect_profile(profile_id), Ok(()));
        assert!(
            state
                .storage_runtime()
                .connection_profiles()
                .value_refs()
                .get_for_profile(&profile_id.to_string())
                .expect("rows")
                .is_empty(),
            "manager save removes the broken row"
        );
        let reloaded = AppStateEntity::new_with_storage_runtime(
            StorageRuntime::for_path(state.storage_runtime().datatab_db_path().to_path_buf())
                .expect("reopen saved database"),
        )
        .expect("reload");
        assert!(reloaded.value_ref_diagnostics(profile_id).is_empty());
        assert_eq!(reloaded.validate_connect_profile(profile_id), Ok(()));
    });
    assert!(window.root(&mut cx).is_err(), "saved form closes");
}

struct PanicOnReadSecretStore;
impl datatab_core::SecretStore for PanicOnReadSecretStore {
    fn is_available(&self) -> bool {
        true
    }
    fn get(&self, _: &str) -> Result<Option<SecretString>, datatab_core::DbError> {
        panic!("flagged profile read a secret")
    }
    fn set(&self, _: &str, _: &SecretString) -> Result<(), datatab_core::DbError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<(), datatab_core::DbError> {
        Ok(())
    }
}

fn flagged_state(cx: &mut TestAppContext) -> (Entity<AppStateEntity>, Uuid) {
    flagged_state_for_fields(cx, &["password", "host"])
}

fn flagged_state_for_fields(
    cx: &mut TestAppContext,
    fields: &[&str],
) -> (Entity<AppStateEntity>, Uuid) {
    let runtime = StorageRuntime::in_memory().expect("storage");
    let mut profile = ConnectionProfile::new("broken source", DbConfig::default_postgres());
    profile.save_password = true;
    for &field in fields {
        profile.value_refs.insert(
            field.into(),
            datatab_core::ValueRef::dotenv("unused", "VALUE"),
        );
    }
    datatab_app::config_loader::save_profiles(&runtime, std::slice::from_ref(&profile))
        .expect("save");
    runtime
        .open_datatab_db()
        .expect("db")
        .execute(
            "UPDATE cfg_connection_profile_value_refs SET env_key = NULL",
            [],
        )
        .expect("break rows");
    let state = cx
        .update(|cx| cx.new(|_| AppStateEntity::new_with_storage_runtime(runtime).expect("load")));
    state.update(cx, |state, _| {
        *state.secret_store().write().expect("store") = Box::new(PanicOnReadSecretStore)
    });
    (state, profile.id)
}

#[::core::prelude::v1::test]
fn broken_value_ref_open_reads_no_secret_and_leaves_fields_empty() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let (state, _) = flagged_state(&mut cx);
    let profile = state.read_with(&cx, |state, _| state.profiles()[0].clone());
    let window = cx
        .update(|cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| ConnectionManagerWindow::new_for_edit(state, &profile, window, cx))
            })
        })
        .expect("open");
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            assert!(manager.form.input_password.read(cx).value().is_empty());
            assert!(
                manager.form.driver_inputs["host"]
                    .read(cx)
                    .value()
                    .is_empty()
            );
            assert!(manager.secrets_read_task.is_none());
            window.remove_window();
        })
        .expect("inspect form");
}

#[::core::prelude::v1::test]
fn broken_value_ref_notice_renders_fields_and_save_first() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let (state, _) = flagged_state(&mut cx);
    // This test measures the notice; the secret-read test separately enforces the keyring gate.
    state.update(&mut cx, |state, _| {
        *state.secret_store().write().expect("store") =
            Box::new(SecretStoreFixture::new(PasswordSaveOutcome::Success))
    });
    let profile = state.read_with(&cx, |state, _| state.profiles()[0].clone());
    let capture = Arc::new(FrameCapture::default());
    let window = cx
        .update(|cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| ConnectionManagerWindow::new_for_edit(state, &profile, window, cx))
            })
        })
        .expect("open");
    window
        .update(&mut cx, |_, window, _| {
            window.observe_frames(&capture);
            window.refresh();
        })
        .expect("observe");
    cx.run_until_parked();
    let frame = capture.0.lock().expect("capture").clone().expect("frame");
    assert!(
        datatab_components::accessibility::accessibility_node(&frame, "broken-value-ref-notice")
            .is_some()
    );
    let text =
        datatab_components::accessibility::accessible_name(&frame, "broken-value-ref-notice")
            .expect("notice text");
    for expected in ["host", "password", "save first", "cannot connect"] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close");
}

#[::core::prelude::v1::test]
fn broken_value_ref_manager_save_removes_row_and_diagnostic_after_reload() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let runtime = StorageRuntime::in_memory().expect("storage");
    let mut profile = sqlite_profile("repair");
    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::dotenv("unused", "PASSWORD"),
    );
    datatab_app::config_loader::save_profiles(&runtime, std::slice::from_ref(&profile))
        .expect("save");
    runtime
        .open_datatab_db()
        .expect("db")
        .execute(
            "UPDATE cfg_connection_profile_value_refs SET env_key = NULL",
            [],
        )
        .expect("break row");
    let profile_id = profile.id;
    let state = cx
        .update(|cx| cx.new(|_| AppStateEntity::new_with_storage_runtime(runtime).expect("load")));
    state.update(&mut cx, |state, _| {
        *state.secret_store().write().expect("store") =
            Box::new(SecretStoreFixture::new(PasswordSaveOutcome::Success))
    });
    let profile = state.read_with(&cx, |state, _| state.profiles()[0].clone());
    let window = open_edit_profile_window(state.clone(), profile, &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            manager.save_profile(window, cx)
        })
        .expect("save repair");
    assert!(window.root(&mut cx).is_err(), "save closes the window");
    state.read_with(&cx, |state, _| {
        assert!(state.value_ref_diagnostics(profile_id).is_empty());
        let rows = state
            .storage_runtime()
            .connection_profiles()
            .value_refs()
            .get_for_profile(&profile_id.to_string())
            .expect("rows");
        assert!(rows.is_empty(), "manager save removes the broken row");
        let reloaded = AppStateEntity::new_with_storage_runtime(
            StorageRuntime::for_path(state.storage_runtime().datatab_db_path().to_path_buf())
                .expect("reopen"),
        )
        .expect("reload");
        assert!(reloaded.value_ref_diagnostics(profile_id).is_empty());
        assert_eq!(reloaded.validate_connect_profile(profile_id), Ok(()));
    });
}

fn flagged_ssh_form(
    cx: &mut TestAppContext,
) -> (
    Entity<AppStateEntity>,
    WindowHandle<ConnectionManagerWindow>,
    Uuid,
) {
    let (state, _) = flagged_state(cx);
    let mut tunnel = datatab_core::SshTunnelProfile::new(
        "fixture tunnel",
        datatab_core::SshTunnelConfig {
            host: "127.0.0.1".into(),
            port: 22,
            user: "fixture".into(),
            auth_method: datatab_core::SshAuthMethod::Password,
            ssh_config_host: None,
        },
    );
    tunnel.save_secret = true;
    let tunnel_id = tunnel.id;
    state.update(cx, |state, _| state.facade.ssh_tunnels.items.push(tunnel));
    let profile = state.read_with(cx, |state, _| state.profiles()[0].clone());
    let window = cx
        .update(|cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| {
                    ConnectionManagerWindow::new_for_edit(state.clone(), &profile, window, cx)
                })
            })
        })
        .expect("open");
    cx.run_until_parked();
    (state, window, tunnel_id)
}

#[::core::prelude::v1::test]
fn broken_value_ref_ssh_test_reads_no_bound_secret() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let (_, window, tunnel_id) = flagged_ssh_form(&mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            manager.access.ssh_enabled = true;
            manager.access.selected_ssh_tunnel_id = Some(tunnel_id);
            manager.test_ssh_connection(window, cx);
            assert!(matches!(manager.ssh_test_status, TestStatus::Failed));
            assert!(
                manager
                    .ssh_test_error
                    .as_deref()
                    .expect("banner")
                    .contains("save first")
            );
            window.remove_window();
        })
        .expect("refuse SSH test");
}

#[::core::prelude::v1::test]
fn broken_value_ref_ssh_selection_reads_no_secret_or_prefill() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let (_, window, tunnel_id) = flagged_ssh_form(&mut cx);
    window
        .update(&mut cx, |manager, _, cx| {
            manager.pending.ssh_tunnel_selection = Some(tunnel_id);
            cx.notify();
        })
        .expect("choose tunnel");
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            assert_eq!(manager.access.selected_ssh_tunnel_id, Some(tunnel_id));
            assert!(
                manager
                    .access
                    .input_ssh_password
                    .read(cx)
                    .value()
                    .is_empty()
            );
            window.remove_window();
        })
        .expect("inspect selection");
}

#[::core::prelude::v1::test]
fn broken_value_ref_ssh_profile_cannot_be_created_from_flagged_form() {
    let mut cx = TestAppContext::single();
    let host = init_form_test_runtime(&mut cx);
    let (state, window, tunnel_id) = flagged_ssh_form(&mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            let tunnel = state
                .read(cx)
                .ssh_tunnels()
                .iter()
                .find(|tunnel| tunnel.id == tunnel_id)
                .expect("tunnel")
                .clone();
            manager.apply_ssh_tunnel(&tunnel, None, window, cx);
            manager.save_current_ssh_as_tunnel(cx);
            assert_eq!(state.read(cx).ssh_tunnels().len(), 1, "no derived profile");
            assert_eq!(host.read(cx).toast_count(), 1, "one Config refusal");
            window.remove_window();
        })
        .expect("refuse profile copy");
}

#[derive(Clone, Default)]
struct SecretMutations {
    writes: Arc<std::sync::atomic::AtomicUsize>,
    deletes: Arc<std::sync::atomic::AtomicUsize>,
}

impl datatab_core::SecretStore for SecretMutations {
    fn is_available(&self) -> bool {
        true
    }
    fn get(&self, _: &str) -> Result<Option<SecretString>, datatab_core::DbError> {
        panic!("broken password repair read a secret")
    }
    fn set(&self, _: &str, _: &SecretString) -> Result<(), datatab_core::DbError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<(), datatab_core::DbError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn broken_password_form(
    cx: &mut TestAppContext,
) -> (
    Entity<AppStateEntity>,
    WindowHandle<ConnectionManagerWindow>,
    Uuid,
    SecretMutations,
) {
    let (state, profile_id) = flagged_state_for_fields(cx, &["password"]);
    let mutations = SecretMutations::default();
    state.update(cx, |state, _| {
        *state.secret_store().write().expect("store") = Box::new(mutations.clone());
    });
    let profile = state.read_with(cx, |state, _| state.profiles()[0].clone());
    let window = open_untouched_edit_window(state.clone(), profile, cx);
    cx.run_until_parked();
    window
        .update(cx, |manager, _, cx| {
            assert!(manager.form.input_password.read(cx).value().is_empty());
            assert!(manager.form.form_save_password);
        })
        .expect("untouched password fixture");
    (state, window, profile_id, mutations)
}

#[::core::prelude::v1::test]
fn broken_value_ref_untouched_password_save_is_refused_without_writes() {
    let mut cx = TestAppContext::single();
    let host = init_form_test_runtime(&mut cx);
    let (state, window, profile_id, mutations) = broken_password_form(&mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            manager
                .form
                .input_name
                .update(cx, |input, cx| input.set_value("refused edit", window, cx));
            manager.save_profile(window, cx);
            assert!(manager.validation_errors.contains(&datatab_i18n::t!(
                "form.validation.broken_password_unresolved"
            )));
        })
        .expect("refused form stays open");
    assert!(window.root(&mut cx).is_ok());
    assert_eq!(mutations.writes.load(Ordering::SeqCst), 0);
    assert_eq!(mutations.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(host.read_with(&cx, |host, _| host.toast_count()), 0);
    state.read_with(&cx, |state, _| {
        assert_eq!(state.profiles()[0].name, "broken source");
        assert!(!state.value_ref_diagnostics(profile_id).is_empty());
        let reloaded = AppStateEntity::new_with_storage_runtime(
            StorageRuntime::for_path(state.storage_runtime().datatab_db_path().to_path_buf())
                .expect("reopen"),
        )
        .expect("reload");
        assert_eq!(reloaded.profiles()[0].name, "broken source");
        assert!(!reloaded.value_ref_diagnostics(profile_id).is_empty());
        let rows = reloaded
            .storage_runtime()
            .connection_profiles()
            .value_refs()
            .get_for_profile(&profile_id.to_string())
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].load_diagnostic().is_some());
    });
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close");
}

#[derive(Clone, Copy)]
enum PasswordRepair {
    Typed,
    Source,
    Prompt,
}

fn save_broken_password_resolution(repair: PasswordRepair) {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let (state, window, profile_id, mutations) = broken_password_form(&mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            match repair {
                PasswordRepair::Typed => manager.form.input_password.update(cx, |input, cx| {
                    input.set_value("replacement fixture", window, cx)
                }),
                PasswordRepair::Source => {
                    manager
                        .form
                        .password_value_source_selector
                        .update(cx, |selector, cx| {
                            selector.set_value_ref(
                                Some(&ValueRef::env("REPLACEMENT_PASSWORD")),
                                window,
                                cx,
                            );
                        });
                    manager.form.input_password.update(cx, |input, cx| {
                        input.set_value("REPLACEMENT_PASSWORD", window, cx)
                    });
                }
                PasswordRepair::Prompt => manager.form.form_save_password = false,
            }
            manager.save_profile(window, cx);
        })
        .expect("save repair");
    assert!(window.root(&mut cx).is_err(), "repair closes form");
    assert_eq!(
        mutations.writes.load(Ordering::SeqCst),
        usize::from(matches!(repair, PasswordRepair::Typed))
    );
    assert_eq!(
        mutations.deletes.load(Ordering::SeqCst),
        0,
        "unseen entry is retained"
    );
    state.read_with(&cx, |state, _| {
        let rows = state
            .storage_runtime()
            .connection_profiles()
            .value_refs()
            .get_for_profile(&profile_id.to_string())
            .expect("rows");
        assert!(rows.iter().all(|row| row.load_diagnostic().is_none()));
        assert_eq!(
            rows.len(),
            usize::from(matches!(repair, PasswordRepair::Source))
        );
        let reloaded = AppStateEntity::new_with_storage_runtime(
            StorageRuntime::for_path(state.storage_runtime().datatab_db_path().to_path_buf())
                .expect("reopen"),
        )
        .expect("reload");
        *reloaded.secret_store().write().expect("store") = Box::new(PanicOnReadSecretStore);
        assert!(state.value_ref_diagnostics(profile_id).is_empty());
        assert!(reloaded.value_ref_diagnostics(profile_id).is_empty());
        assert_eq!(reloaded.validate_connect_profile(profile_id), Ok(()));
        if !matches!(repair, PasswordRepair::Typed) {
            let prepared = reloaded
                .prepare_connect_profile(profile_id)
                .expect("prepare without keyring reads");
            assert!(prepared.password_override.is_none());
            assert_eq!(
                reloaded.connection_password_prompt(profile_id).is_some(),
                matches!(repair, PasswordRepair::Prompt)
            );
        }
    });
}

#[::core::prelude::v1::test]
fn broken_value_ref_typed_password_save_clears_diagnostic_after_reload() {
    save_broken_password_resolution(PasswordRepair::Typed);
}

#[::core::prelude::v1::test]
fn broken_value_ref_source_save_prepares_without_keyring_after_reload() {
    save_broken_password_resolution(PasswordRepair::Source);
}

#[::core::prelude::v1::test]
fn broken_value_ref_save_password_off_prepares_without_keyring_after_reload() {
    save_broken_password_resolution(PasswordRepair::Prompt);
}

#[::core::prelude::v1::test]
fn broken_value_ref_password_placeholder_asks_for_value_or_source() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let (_, window, _, _) = broken_password_form(&mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            assert_eq!(
                manager.password_placeholder(true, cx),
                "This field needs a value or a source"
            );
            window.remove_window();
        })
        .expect("inspect placeholder");
}

#[::core::prelude::v1::test]
fn broken_value_ref_env_whitespace_password_save_is_refused_without_writes() {
    let mut cx = TestAppContext::single();
    let host = init_form_test_runtime(&mut cx);
    let (state, window, profile_id, mutations) = broken_password_form(&mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            manager
                .form
                .password_value_source_selector
                .update(cx, |selector, cx| {
                    selector.set_value_ref(Some(&ValueRef::env("PASSWORD")), window, cx);
                });
            manager
                .form
                .input_password
                .update(cx, |input, cx| input.set_value("   ", window, cx));
            manager
                .form
                .input_name
                .update(cx, |input, cx| input.set_value("refused edit", window, cx));
            assert!(manager.form.form_save_password);
            assert!(
                !manager
                    .form
                    .password_value_source_selector
                    .read(cx)
                    .is_literal(cx)
            );
            assert!(!manager.collect_value_refs(cx).contains_key("password"));
            manager.save_profile(window, cx);
            assert!(manager.validation_errors.contains(&datatab_i18n::t!(
                "form.validation.broken_password_unresolved"
            )));
        })
        .expect("refused form stays open");
    assert!(window.root(&mut cx).is_ok());
    assert_eq!(mutations.writes.load(Ordering::SeqCst), 0);
    assert_eq!(mutations.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(host.read_with(&cx, |host, _| host.toast_count()), 0);
    state.read_with(&cx, |state, _| {
        assert_eq!(state.profiles()[0].name, "broken source");
        assert!(!state.value_ref_diagnostics(profile_id).is_empty());
        let reloaded = AppStateEntity::new_with_storage_runtime(
            StorageRuntime::for_path(state.storage_runtime().datatab_db_path().to_path_buf())
                .expect("reopen"),
        )
        .expect("reload");
        assert_eq!(reloaded.profiles()[0].name, "broken source");
        assert!(!reloaded.value_ref_diagnostics(profile_id).is_empty());
        let rows = reloaded
            .storage_runtime()
            .connection_profiles()
            .value_refs()
            .get_for_profile(&profile_id.to_string())
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].load_diagnostic().is_some());
    });
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close");
}

#[::core::prelude::v1::test]
fn broken_value_ref_password_placeholder_preserves_source_and_field_hints() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    for field in ["host", "password"] {
        let (state, _) = flagged_state_for_fields(&mut cx, &[field]);
        let profile = state.read_with(&cx, |state, _| state.profiles()[0].clone());
        let window = open_untouched_edit_window(state, profile, &mut cx);
        cx.run_until_parked();
        window
            .update(&mut cx, |manager, window, cx| {
                let literal_placeholder = if field == "password" {
                    datatab_i18n::t!("connection_manager.placeholder.password_needs_value")
                } else {
                    datatab_i18n::t!("connection_manager.placeholder.password_unreadable")
                };
                assert_eq!(manager.password_placeholder(true, cx), literal_placeholder);
                for (source, expected) in [
                    (
                        ValueRef::env("PASSWORD"),
                        datatab_i18n::t!("components.value_source.placeholder.env"),
                    ),
                    (
                        ValueRef::dotenv("unused", "PASSWORD"),
                        datatab_i18n::t!("components.value_source.placeholder.dotenv"),
                    ),
                    (
                        ValueRef::file("unused", None),
                        datatab_i18n::t!("components.value_source.placeholder.file"),
                    ),
                ] {
                    manager
                        .form
                        .password_value_source_selector
                        .update(cx, |selector, cx| {
                            selector.set_value_ref(Some(&source), window, cx);
                        });
                    assert!(
                        !manager
                            .form
                            .password_value_source_selector
                            .read(cx)
                            .is_literal(cx)
                    );
                    assert_eq!(manager.password_placeholder(false, cx), expected);
                }
                window.remove_window();
            })
            .expect("inspect source and field placeholders");
    }
}
