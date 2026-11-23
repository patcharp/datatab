use super::super::super::{ImportConnectionsPanel, ImportRunResult, Step};
use super::*;
use datatab_ui_base::AppStateEntity;
use datatab_ui_base::toast::{ToastGlobal, ToastHost};
use gpui::{AppContext, Entity, PlatformDispatcher, TestAppContext, WindowHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, mpsc};

struct CheckedKeyring {
    keyring: Keyring,
    dispatcher: gpui::TestDispatcher,
    foreground_calls: Arc<AtomicUsize>,
    gate: Option<WriteGate>,
}

#[derive(Clone)]
struct WriteGate {
    released: Arc<(Mutex<bool>, Condvar)>,
    entered: mpsc::Sender<()>,
    writes: Arc<AtomicUsize>,
}

impl CheckedKeyring {
    fn check_executor(&self) {
        if self.dispatcher.is_main_thread() {
            self.foreground_calls.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl SecretStore for CheckedKeyring {
    fn is_available(&self) -> bool {
        true
    }
    fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, DbError> {
        self.check_executor();
        self.keyring.get(secret_ref)
    }
    #[expect(clippy::unwrap_in_result, reason = "test synchronization must succeed")]
    fn set(&self, secret_ref: &str, secret: &SecretString) -> Result<(), DbError> {
        self.check_executor();
        if let Some(gate) = &self.gate {
            assert!(
                !self.dispatcher.is_main_thread(),
                "import writes must be in the background"
            );
            gate.writes.fetch_add(1, Ordering::SeqCst);
            gate.entered.send(()).expect("receiver");
            let (released, ready) = &*gate.released;
            let mut released = released.lock().expect("gate");
            while !*released {
                released = ready.wait(released).expect("release write");
            }
        }
        self.keyring.set(secret_ref, secret)
    }
    fn delete(&self, secret_ref: &str) -> Result<(), DbError> {
        self.check_executor();
        self.keyring.delete(secret_ref)
    }
}

struct ImportFixture {
    app_state: Entity<AppStateEntity>,
    panel: Entity<ImportConnectionsPanel>,
    window: WindowHandle<gpui_component::Root>,
    host: Entity<ToastHost>,
    keyring: Keyring,
    foreground_calls: Arc<AtomicUsize>,
}

impl ImportFixture {
    fn new(cx: &mut TestAppContext, fail_password: bool, external: bool) -> Self {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        let host = cx.new(|_| ToastHost::new());
        cx.update(|cx| cx.set_global(ToastGlobal { host: host.clone() }));
        let keyring = Keyring {
            fail_password,
            ..Keyring::default()
        };
        let app_state = cx.new(|_| {
            AppStateEntity::new_with_storage_runtime(
                datatab_storage::bootstrap::StorageRuntime::in_memory().expect("real SQLite"),
            )
            .expect("app state")
        });
        let foreground_calls = Arc::new(AtomicUsize::new(0));
        let dispatcher = cx.dispatcher.clone();
        app_state.update(cx, |state, _| {
            let metadata = datatab_core::DriverMetadataBuilder::new(
                "import",
                "Import",
                datatab_core::DatabaseCategory::Relational,
                datatab_core::QueryLanguage::Sql,
            )
            .build();
            state.register_driver_for_test("rpc:import", Arc::new(RpcDriver(metadata)));
            state.facade.secrets = datatab_core::SecretManager::new(Box::new(CheckedKeyring {
                keyring: keyring.clone(),
                dispatcher,
                foreground_calls: foreground_calls.clone(),
                gate: None,
            }));
        });
        if external {
            app_state.update(cx, |state, _| {
                let metadata = datatab_core::DriverMetadataBuilder::new(
                    "sqlite",
                    "SQLite",
                    datatab_core::DatabaseCategory::Relational,
                    datatab_core::QueryLanguage::Sql,
                )
                .build();
                state.register_driver_for_test("sqlite", Arc::new(RpcDriver(metadata)));
            });
        }
        let mut panel = None;
        let window = cx.add_window(|window, cx| {
            let entity = cx.new(|cx| ImportConnectionsPanel::new(app_state.clone(), window, cx));
            panel = Some(entity.clone());
            gpui_component::Root::new(entity, window, cx)
        });
        let panel = panel.expect("panel");
        window
            .update(cx, |_, window, cx| {
                panel.update(cx, |panel, cx| {
                    configure_import(panel, external);
                    window.focus(panel.focus.handle(), cx);
                })
            })
            .expect("configure panel");
        cx.run_until_parked();
        Self {
            app_state,
            panel,
            window,
            host,
            keyring,
            foreground_calls,
        }
    }

    fn mount_manager(
        &mut self,
        cx: &mut TestAppContext,
        external: bool,
    ) -> Entity<crate::connection_manager::ConnectionManagerWindow> {
        use crate::connection_manager::ConnectionManagerWindow;
        cx.update(datatab_ui_base::keymap::init_keymap);
        self.window
            .update(cx, |_, window, _| window.remove_window())
            .expect("remove panel host");
        let mut manager = None;
        self.window = cx.add_window(|window, cx| {
            let entity =
                cx.new(|cx| ConnectionManagerWindow::new(self.app_state.clone(), window, cx));
            self.panel = entity.read(cx).import_panel.clone();
            entity.update(cx, |manager, cx| manager.open_import(window, cx));
            manager = Some(entity.clone());
            gpui_component::Root::new(entity, window, cx)
        });
        let manager = manager.expect("manager");
        self.window
            .update(cx, |_, window, cx| {
                self.panel.update(cx, |panel, cx| {
                    configure_import(panel, external);
                    window.focus(panel.focus.handle(), cx);
                    cx.notify();
                });
            })
            .expect("configure manager import");
        cx.run_until_parked();
        manager
    }

    fn start(&self, cx: &mut TestAppContext, external: bool) {
        self.window
            .update(cx, |_, window, cx| {
                self.panel.update(cx, |panel, cx| {
                    if external {
                        panel.do_apply_external(window, cx);
                    } else {
                        panel.do_apply(window, cx);
                    }
                })
            })
            .expect("start import");
    }
}

struct BackgroundWorker {
    released: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for BackgroundWorker {
    fn drop(&mut self) {
        let (released, ready) = &*self.released;
        *released.lock().expect("release gate") = true;
        ready.notify_all();
        if let Some(worker) = self.worker.take() {
            worker.join().expect("background import completes");
        }
    }
}

fn block_writes(
    fixture: &ImportFixture,
    cx: &mut TestAppContext,
) -> (WriteGate, mpsc::Receiver<()>) {
    let (entered, receiver) = mpsc::channel();
    let gate = WriteGate {
        released: Arc::new((Mutex::new(false), Condvar::new())),
        entered,
        writes: Arc::new(AtomicUsize::new(0)),
    };
    let dispatcher = cx.dispatcher.clone();
    fixture.app_state.update(cx, |state, _| {
        *state.secret_store().write().expect("store") = Box::new(CheckedKeyring {
            keyring: fixture.keyring.clone(),
            dispatcher,
            foreground_calls: fixture.foreground_calls.clone(),
            gate: Some(gate.clone()),
        });
    });
    (gate, receiver)
}

fn wait_for_write(
    cx: &TestAppContext,
    gate: &WriteGate,
    receiver: &mpsc::Receiver<()>,
) -> BackgroundWorker {
    let dispatcher = cx.dispatcher.clone();
    let worker = BackgroundWorker {
        released: gate.released.clone(),
        worker: Some(std::thread::spawn(move || while dispatcher.tick(true) {})),
    };
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a keyring write must block before the deadline");
    worker
}

fn run_blocked_import(external: bool, close: bool, fail_password: bool) {
    let mut cx = TestAppContext::single();
    let fixture = ImportFixture::new(&mut cx, fail_password, external);
    let (gate, receiver) = block_writes(&fixture, &mut cx);
    fixture.start(&mut cx, external);
    let worker = wait_for_write(&cx, &gate, &receiver);
    let observations = fixture
        .window
        .update(&mut cx, |_, window, cx| {
            fixture.panel.update(cx, |panel, cx| {
                let busy = panel.is_busy();
                panel.file_input.update(cx, |input, cx| {
                    input.set_value("still editable", window, cx)
                });
                configure_import(panel, external);
                if external {
                    panel.do_apply_external(window, cx);
                } else {
                    panel.do_apply(window, cx);
                }
                let second_ignored = if external {
                    panel.external_candidates.len() == 1
                } else {
                    panel.parsed_bundle.is_some()
                };
                if close {
                    window.remove_window();
                }
                (busy, second_ignored)
            })
        })
        .expect("the UI remains usable while the keyring is blocked");
    let rows_before_release = fixture.app_state.read_with(&cx, |state, _| {
        (
            state.profiles().len(),
            state
                .storage_runtime()
                .connection_profiles()
                .all()
                .expect("SQLite rows")
                .len(),
        )
    });
    drop(worker);
    cx.run_until_parked();
    assert_eq!(
        observations,
        (true, true),
        "busy panel ignores a second import"
    );
    assert_eq!(rows_before_release, (0, 0), "secrets precede the state row");
    assert_eq!(fixture.foreground_calls.load(Ordering::SeqCst), 0);
    assert_completed_import(&fixture, &cx, external, close, fail_password);
    assert_eq!(
        gate.writes.load(Ordering::SeqCst),
        if external { 1 } else { 2 },
        "one import ran"
    );
}

fn assert_completed_import(
    fixture: &ImportFixture,
    cx: &TestAppContext,
    external: bool,
    close: bool,
    fail_password: bool,
) {
    let expected_rows = usize::from(!fail_password || external);
    fixture.app_state.read_with(cx, |state, _| {
        assert_eq!(state.profiles().len(), expected_rows);
        assert_eq!(
            state
                .storage_runtime()
                .connection_profiles()
                .all()
                .expect("SQLite rows")
                .len(),
            expected_rows
        );
    });
    if fail_password && !external {
        assert!(
            fixture.keyring.values.lock().expect("keyring").is_empty(),
            "failed password restores fields"
        );
    }
    if close {
        assert_eq!(
            fixture.host.read_with(cx, |host, _| host.toast_count()),
            usize::from(fail_password),
            "closed-window failure is reported once"
        );
        if fail_password && !external {
            assert_eq!(
                fixture
                    .host
                    .read_with(cx, |host, _| host.last_toast_title()),
                Some(datatab_i18n::t!(
                    "connection_manager.import.error.password_keyring_failed"
                )),
                "closed-window reports keep the actual failure reason"
            );
        }
    } else {
        fixture.panel.read_with(cx, |panel, _| {
            assert!(!panel.is_busy());
            match &panel.run_result {
                Some(ImportRunResult::Outcome(outcome)) => {
                    assert_eq!(
                        outcome.succeeded,
                        if fail_password {
                            Vec::<String>::new()
                        } else {
                            vec!["in config".to_string()]
                        }
                    );
                    assert_eq!(outcome.config_failures.len(), usize::from(fail_password));
                    if fail_password {
                        assert_eq!(
                            outcome.config_failures[0].1,
                            datatab_i18n::t!(
                                "connection_manager.import.error.password_keyring_failed"
                            )
                        );
                    }
                    assert!(outcome.secret_failures.is_empty());
                    assert!(outcome.needs_driver.is_empty());
                    assert!(outcome.unresolved_refs.is_empty());
                }
                Some(ImportRunResult::External(outcome)) => {
                    assert_eq!(outcome.succeeded, ["external import"]);
                    assert_eq!(outcome.secret_failures.len(), usize::from(fail_password));
                    assert!(outcome.config_failures.is_empty());
                    assert!(outcome.needs_driver.is_empty());
                }
                _ => panic!("import outcome"),
            }
        });
    }
}

#[test]
fn blocked_bundle_import_stays_usable_and_ignores_a_second_run() {
    run_blocked_import(false, false, false);
}

#[test]
fn blocked_bundle_password_failure_rolls_back_its_fields() {
    run_blocked_import(false, false, true);
}

#[test]
fn closed_window_import_finishes_and_reports_failure_once() {
    run_blocked_import(false, true, true);
}

#[test]
fn closed_window_successful_import_finishes() {
    run_blocked_import(false, true, false);
}

#[test]
fn blocked_external_import_stays_usable_and_ignores_a_second_run() {
    run_blocked_import(true, false, false);
}

#[test]
fn closed_window_external_import_reports_secret_failure_once() {
    run_blocked_import(true, true, true);
}

fn configure_import(panel: &mut ImportConnectionsPanel, external: bool) {
    if external {
        // A built-in config follows the external-client importer's ordinary password path.
        panel.external_candidates = vec![datatab_portability::external::ExternalImportCandidate {
            name: "external import".into(),
            config: DbConfig::default_sqlite(),
            kind: DbKind::SQLite,
            secret: Some(SecretString::from("sample password")),
            secret_skip_reason: None,
        }];
        panel.external_includes = vec![true];
        panel.step = Step::ExternalReview;
        return;
    }
    let source_keyring = Keyring::default();
    let mut source = state(&source_keyring);
    let mut profile = bundled_profiles().remove(0);
    if let DbConfig::External { values, .. } = &mut profile.config {
        values.insert("password".into(), String::new());
    }
    let _changes = source
        .prepare_external_secret_fields(&mut profile)
        .apply()
        .expect("source fields");
    source_keyring
        .set(
            &profile.secret_ref(),
            &SecretString::from("sample password"),
        )
        .expect("source password");
    source.add_profile_in_folder(profile, None);
    let mut parsed = datatab_portability::import::parse(&export_bundle(&source, &source_keyring))
        .expect("bundle");
    datatab_portability::import::decrypt(&mut parsed, &SecretString::from(String::new()))
        .expect("open plaintext secrets");
    let plan = datatab_portability::import::plan(
        &parsed,
        &datatab_portability::DestSnapshot {
            auth_profiles: vec![],
            ssh_tunnels: vec![],
            proxies: vec![],
            connections: vec![],
        },
    );
    for resolution in &plan.required_resolutions {
        panel.secret_values.insert(
            (resolution.owner_local_id.clone(), resolution.field.clone()),
            "sample value".into(),
        );
    }
    panel.parsed_bundle = Some(parsed);
    panel.import_plan = Some(plan);
    panel.step = Step::RequiredReferences;
}

#[test]
fn bundle_import_keyring_and_password_rollback_use_the_background_executor() {
    for fail_password in [false, true] {
        let mut cx = TestAppContext::single();
        let fixture = ImportFixture::new(&mut cx, fail_password, false);
        fixture.start(&mut cx, false);
        cx.run_until_parked();
        assert_eq!(
            fixture.foreground_calls.load(Ordering::SeqCst),
            0,
            "import keyring calls must use the background executor"
        );
        fixture.app_state.read_with(&cx, |state, _| {
            assert_eq!(state.profiles().len(), usize::from(!fail_password));
        });
        fixture.panel.read_with(&cx, |panel, _| {
            assert!(!panel.is_busy());
            let Some(ImportRunResult::Outcome(outcome)) = &panel.run_result else {
                panic!("bundle outcome")
            };
            assert_eq!(outcome.config_failures.len(), usize::from(fail_password));
            assert!(outcome.secret_failures.is_empty());
        });
        if fail_password {
            assert!(
                fixture.keyring.values.lock().expect("keyring").is_empty(),
                "failed password rolls back fields"
            );
        }
    }
}

#[test]
fn external_import_keyring_uses_the_background_executor() {
    let mut cx = TestAppContext::single();
    let fixture = ImportFixture::new(&mut cx, false, true);
    fixture.app_state.update(&mut cx, |state, _| {
        // This driver is registered only for its built-in identity; no database is opened.
        let metadata = datatab_core::DriverMetadataBuilder::new(
            "sqlite",
            "SQLite",
            datatab_core::DatabaseCategory::Relational,
            datatab_core::QueryLanguage::Sql,
        )
        .build();
        state.register_driver_for_test("sqlite", Arc::new(RpcDriver(metadata)));
    });
    fixture.start(&mut cx, true);
    cx.run_until_parked();
    assert_eq!(
        fixture.foreground_calls.load(Ordering::SeqCst),
        0,
        "external import keyring calls must use the background executor"
    );
    fixture
        .app_state
        .read_with(&cx, |state, _| assert_eq!(state.profiles().len(), 1));
}

#[test]
fn busy_import_escape_keeps_the_manager_and_rejects_reopening() {
    use crate::connection_manager::View;

    for external in [false, true] {
        let mut cx = TestAppContext::single();
        let mut fixture = ImportFixture::new(&mut cx, false, external);
        let manager = fixture.mount_manager(&mut cx, external);
        let (gate, receiver) = block_writes(&fixture, &mut cx);
        fixture.start(&mut cx, external);
        let worker = wait_for_write(&cx, &gate, &receiver);
        let before_escape = fixture.panel.read_with(&cx, |panel, _| {
            (panel.step, panel.is_applying, panel.is_applying_external)
        });
        cx.dispatch_keystroke(
            fixture.window.into(),
            gpui::Keystroke::parse("escape").expect("Escape"),
        );
        assert!(
            manager.read_with(&cx, |manager, _| manager.view == View::Import),
            "Escape must keep the busy import view"
        );
        assert_eq!(
            fixture.panel.read_with(&cx, |panel, _| {
                (panel.step, panel.is_applying, panel.is_applying_external)
            }),
            before_escape,
            "Escape must keep the import state"
        );
        fixture
            .window
            .update(&mut cx, |_, window, cx| {
                manager.update(cx, |manager, cx| {
                    manager.open_import(window, cx);
                    manager.open_import_external(window, cx);
                });
                fixture.panel.update(cx, |panel, cx| {
                    let before = (panel.step, panel.is_applying, panel.is_applying_external);
                    panel.reset(window, cx);
                    assert_eq!(
                        (panel.step, panel.is_applying, panel.is_applying_external),
                        before
                    );
                    assert_eq!(
                        (panel.is_applying, panel.is_applying_external),
                        (!external, external)
                    );
                    assert_eq!(
                        panel.step,
                        if external {
                            Step::ExternalReview
                        } else {
                            Step::RequiredReferences
                        }
                    );
                    configure_import(panel, external);
                    if external {
                        panel.do_apply_external(window, cx);
                    } else {
                        panel.do_apply(window, cx);
                    }
                    assert!(
                        if external {
                            panel.external_candidates.len() == 1
                        } else {
                            panel.parsed_bundle.is_some()
                        },
                        "second import must leave its input untouched"
                    );
                });
            })
            .expect("busy reopen and reset do nothing");
        drop(worker);
        cx.run_until_parked();
        assert_completed_import(&fixture, &cx, external, false, false);
        assert_eq!(
            gate.writes.load(Ordering::SeqCst),
            if external { 1 } else { 2 }
        );
        assert_eq!(fixture.foreground_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn import_rejects_an_auth_profile_deleted_during_the_keyring_wait() {
    run_deleted_auth_import(false, false, false);
}

#[test]
fn refused_import_reports_password_cleanup_failure_once() {
    run_deleted_auth_import(false, true, false);
}

#[test]
fn refused_import_replaces_its_password_write_failure() {
    run_deleted_auth_import(true, false, false);
}

#[test]
fn refused_import_reports_fields_that_could_not_be_restored() {
    run_deleted_auth_import(false, false, true);
}

#[test]
fn refused_import_reports_password_cleanup_and_field_restore_failures_once() {
    run_deleted_auth_import(false, true, true);
}

fn run_deleted_auth_import(fail_password: bool, fail_delete: bool, fail_restore: bool) {
    let mut cx = TestAppContext::single();
    let fixture = ImportFixture::new(&mut cx, fail_password, false);
    let auth = datatab_core::AuthProfile::new("chosen auth", "test", HashMap::new());
    let auth_id = auth.id;
    fixture.app_state.update(&mut cx, |state, _| {
        assert!(state.add_auth_profile(auth));
    });
    let mut profile = bundled_profiles().remove(0);
    let field_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");
    let with_fields = !fail_password && (!fail_delete || fail_restore);
    if !with_fields && let DbConfig::External { values, .. } = &mut profile.config {
        values.remove("api_token");
    }
    let actions = chosen_auth_actions(profile, auth_id);
    let (gate, receiver) = block_writes(&fixture, &mut cx);
    fixture
        .window
        .update(&mut cx, |_, window, cx| {
            fixture.panel.update(cx, |panel, cx| {
                let mut session = panel.import_session(cx);
                panel.is_applying = true;
                let task = cx.background_executor().spawn(async move {
                    let outcome =
                        datatab_app::portability::persist_import_actions(actions, &mut session);
                    (session, ImportRunResult::Outcome(outcome))
                });
                panel.finish_import_task(task, window, cx);
            });
        })
        .expect("start import with chosen auth");
    let worker = wait_for_write(&cx, &gate, &receiver);
    fixture.app_state.update(&mut cx, |state, _| {
        assert_eq!(
            state.remove_auth_profile(0).expect("deleted auth").id,
            auth_id
        );
    });
    fixture
        .keyring
        .fail_delete
        .store(fail_delete, Ordering::SeqCst);
    drop(worker);
    fixture
        .keyring
        .refuse_field_deletes
        .store(fail_restore, Ordering::SeqCst);
    cx.run_until_parked();
    let mut reason = if fail_delete {
        datatab_i18n::t!("connection_manager.import.error.refused_secret_cleanup_failed")
            .to_string()
    } else {
        format!("the chosen auth profile {auth_id} no longer exists")
    };
    if fail_restore {
        reason.push_str(" The saved value of these secret fields may have changed; check them before connecting: 'API token'.");
    }
    assert_deleted_auth_import(
        &fixture,
        &cx,
        &reason,
        usize::from(fail_delete) + usize::from(fail_restore),
    );
    assert_eq!(fixture.keyring.entry(&field_ref).is_some(), fail_restore);
    assert!(!reason.contains(SENTINEL));
    assert_eq!(fixture.host.read_with(&cx, |host, _| host.toast_count()), 0);
}

fn chosen_auth_actions(
    profile: ConnectionProfile,
    auth_id: uuid::Uuid,
) -> datatab_portability::ImportActions {
    let connection_id = profile.id;
    datatab_portability::ImportActions {
        connections: vec![profile],
        auth_profiles: vec![],
        ssh_tunnels: vec![],
        proxies: vec![],
        secret_writes: vec![(
            datatab_core::connection_secret_ref(&connection_id),
            SecretString::from("sample password"),
        )],
        connection_field_secrets: vec![],
        kind_failures: vec![],
        unresolved_ref_connections: vec![],
        value_ref_failures: vec![],
        chosen_auth_profiles: vec![(connection_id, auth_id)],
    }
}

fn assert_deleted_auth_import(
    fixture: &ImportFixture,
    cx: &TestAppContext,
    reason: &str,
    retained_secrets: usize,
) {
    fixture.app_state.read_with(cx, |state, _| {
        assert!(
            state.profiles().is_empty(),
            "a connection with deleted auth must not commit"
        );
        assert!(
            state
                .storage_runtime()
                .connection_profiles()
                .all()
                .expect("SQLite rows")
                .is_empty()
        );
    });
    fixture.panel.read_with(cx, |panel, _| {
        assert!(!panel.is_busy());
        let Some(ImportRunResult::Outcome(outcome)) = &panel.run_result else {
            panic!("import outcome");
        };
        assert!(outcome.succeeded.is_empty());
        assert_eq!(
            outcome.config_failures,
            vec![("in config".into(), reason.to_string())]
        );
        assert!(outcome.secret_failures.is_empty());
        assert!(outcome.needs_driver.is_empty());
        assert!(outcome.unresolved_refs.is_empty());
    });
    assert_eq!(
        fixture.keyring.values.lock().expect("keyring").len(),
        retained_secrets,
        "the outcome must account for retained secrets"
    );
    assert_eq!(
        fixture.foreground_calls.load(Ordering::SeqCst),
        0,
        "rollback must stay in the background"
    );
}

#[test]
fn busy_guard_keeps_native_import_closed() {
    assert_busy_guard(false);
}

#[test]
fn busy_guard_keeps_external_import_closed() {
    assert_busy_guard(true);
}

fn assert_busy_guard(external: bool) {
    use crate::connection_manager::View;
    for busy_phase in 0..4 {
        let mut cx = TestAppContext::single();
        let mut fixture = ImportFixture::new(&mut cx, false, false);
        let manager = fixture.mount_manager(&mut cx, false);
        fixture
            .window
            .update(&mut cx, |_, window, cx| {
                fixture.panel.update(cx, |panel, _| {
                    panel.is_parsing = busy_phase == 0;
                    panel.is_parsing_external = busy_phase == 1;
                    panel.is_applying = busy_phase == 2;
                    panel.is_applying_external = busy_phase == 3;
                });
                manager.update(cx, |manager, cx| {
                    manager.view = View::DriverSelect;
                    if external {
                        manager.open_import_external(window, cx);
                    } else {
                        manager.open_import(window, cx);
                    }
                    assert!(
                        manager.view == View::DriverSelect,
                        "busy guard must keep the current view"
                    );
                });
                {
                    let panel = fixture.panel.read(cx);
                    assert_eq!(panel.step, Step::RequiredReferences);
                    assert_eq!(
                        panel.source_selection,
                        super::super::super::ImportSourceSelection::Bundle
                    );
                    assert!(panel.is_busy());
                }
            })
            .expect("open import while busy");
    }
}

#[test]
fn closed_window_report_keeps_mixed_failure_reasons_once() {
    let config_reason = "sample configuration failure";
    let expected = [
        config_reason.to_string(),
        crate::labels::import_outcome_needs_driver(1),
        crate::labels::import_outcome_unresolved_refs(1),
        crate::labels::import_error_secret_failures_toast(1),
    ]
    .join("\n");
    let outcomes = [
        (
            ImportRunResult::Outcome(datatab_app::portability::ImportOutcome {
                config_failures: vec![("sample row".into(), config_reason.into())],
                needs_driver: vec![("sample row".into(), "sample driver".into())],
                unresolved_refs: vec!["sample row".into()],
                secret_failures: vec![("sample row".into(), "sample reference".into())],
                ..Default::default()
            }),
            expected,
        ),
        (
            ImportRunResult::External(datatab_app::portability::ExternalPersistOutcome {
                config_failures: vec![("sample row".into(), config_reason.into())],
                ..Default::default()
            }),
            config_reason.to_string(),
        ),
    ];
    for (result, expected) in outcomes {
        let mut cx = TestAppContext::single();
        let fixture = ImportFixture::new(&mut cx, false, false);
        fixture
            .window
            .update(&mut cx, |_, window, cx| {
                fixture.panel.update(cx, |panel, cx| {
                    let session = panel.import_session(cx);
                    let task = cx
                        .background_executor()
                        .spawn(async move { (session, result) });
                    panel.finish_import_task(task, window, cx);
                    window.remove_window();
                });
            })
            .expect("close importing window");
        cx.run_until_parked();
        fixture.host.read_with(&cx, |host, _| {
            assert_eq!(host.toast_count(), 1);
            assert_eq!(host.last_toast_occurrences(), Some(1));
            assert_eq!(host.last_toast_title(), Some(expected));
        });
    }
}
