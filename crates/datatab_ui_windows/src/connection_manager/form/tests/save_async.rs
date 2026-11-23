use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::{Condvar, mpsc};

struct BlockingWrites {
    fixture: SecretStoreFixture,
    foreground: std::thread::ThreadId,
    dispatcher: gpui::TestDispatcher,
    released: Arc<(Mutex<bool>, Condvar)>,
    entered: mpsc::Sender<()>,
    writes: Arc<AtomicUsize>,
    removing_profile: Arc<AtomicBool>,
}

#[expect(clippy::unwrap_in_result, reason = "test synchronization must succeed")]
impl SecretStore for BlockingWrites {
    fn is_available(&self) -> bool {
        true
    }
    fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, datatab_core::DbError> {
        assert_ne!(
            std::thread::current().id(),
            self.foreground,
            "keyring reads must run off the UI thread"
        );
        self.fixture.get(secret_ref)
    }
    fn set(&self, secret_ref: &str, value: &SecretString) -> Result<(), datatab_core::DbError> {
        assert_ne!(
            std::thread::current().id(),
            self.foreground,
            "keyring writes must run off the UI thread"
        );
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).expect("test receiver available");
        let (released, ready) = &*self.released;
        let mut released = released.lock().expect("release lock");
        while !*released {
            released = ready.wait(released).expect("release wait");
        }
        self.fixture.set(secret_ref, value)
    }
    fn delete(&self, secret_ref: &str) -> Result<(), datatab_core::DbError> {
        if !self.removing_profile.load(Ordering::SeqCst) {
            // GPUI's test dispatcher can run background tasks on the test thread.
            assert!(
                !self.dispatcher.is_main_thread(),
                "save cleanup must use the background executor"
            );
        }
        self.fixture.delete(secret_ref)
    }
}

struct ReleaseWrites(Arc<(Mutex<bool>, Condvar)>);
impl Drop for ReleaseWrites {
    fn drop(&mut self) {
        let (released, ready) = &*self.0;
        *released.lock().expect("release lock") = true;
        ready.notify_all();
    }
}

struct SaveFormFixture {
    host: Entity<ToastHost>,
    fixture: SecretStoreFixture,
    app_state: Entity<AppStateEntity>,
    profile: ConnectionProfile,
    secret_ref: String,
    window: WindowHandle<ConnectionManagerWindow>,
}

impl SaveFormFixture {
    fn new(cx: &mut TestAppContext, close: bool) -> Self {
        let host = init_form_test_runtime(cx);
        let fixture = SecretStoreFixture::new(PasswordSaveOutcome::Success);
        let app_state = test_app_state(cx, fixture.clone());
        secret_fields::register_rpc_driver(&app_state, cx);
        let mut profile = secret_fields::rpc_profile();
        profile.keyring_secret_fields.insert("api_token".into());
        let secret_ref = datatab_core::connection_field_secret_ref(&profile.id, "api_token");
        fixture.seed_ref(&secret_ref, "old token");
        app_state.update(cx, |state, _| {
            state.add_profile_in_folder(profile.clone(), None)
        });
        let window = open_untouched_edit_window(app_state.clone(), profile.clone(), cx);
        cx.run_until_parked();
        if close {
            fixture.fail_writes_of(profile.secret_ref());
        }

        Self {
            host,
            fixture,
            app_state,
            profile,
            secret_ref,
            window,
        }
    }
}

// Only background work runs on this helper thread; GPUI entities remain on the test thread.
fn run_blocked_save(close: bool) {
    let mut cx = TestAppContext::single();
    let SaveFormFixture {
        host,
        fixture,
        app_state,
        profile,
        secret_ref,
        window,
    } = SaveFormFixture::new(&mut cx, close);
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let writes = Arc::new(AtomicUsize::new(0));
    let (entered, receiver) = mpsc::channel();
    let dispatcher = cx.dispatcher.clone();
    app_state.update(&mut cx, |state, _| {
        *state.secret_store().write().expect("secret store lock") = Box::new(BlockingWrites {
            fixture: fixture.clone(),
            foreground: std::thread::current().id(),
            dispatcher,
            released: released.clone(),
            entered,
            writes: writes.clone(),
            removing_profile: Arc::new(AtomicBool::new(false)),
        });
    });
    window
        .update(&mut cx, |manager, window, cx| {
            manager
                .form
                .input_name
                .update(cx, |input, cx| input.set_value("saved once", window, cx));
            manager
                .form
                .driver_inputs
                .get("api_token")
                .expect("token input")
                .update(cx, |input, cx| input.set_value("new token", window, cx));
            manager.form.form_save_password = close;
            manager
                .form
                .input_password
                .update(cx, |input, cx| input.set_value("primary", window, cx));
            manager.save_profile(window, cx);
        })
        .expect("save returns while the keyring is blocked");
    let dispatcher = cx.dispatcher.clone();
    let worker = std::thread::spawn(move || while dispatcher.tick(true) {});
    let release = ReleaseWrites(released);
    receiver.recv().expect("background write reached the store");
    window
        .update(&mut cx, |manager, window, cx| {
            assert!(manager.saving, "the form shows a save in flight");
            manager.form.input_name.update(cx, |input, cx| {
                input.set_value("second save ignored", window, cx)
            });
            manager.save_profile(window, cx);
            if close {
                window.remove_window();
            }
        })
        .expect("the form entity can be updated while the keyring is blocked");
    assert_eq!(
        app_state.read_with(&cx, |state, _| state.profiles()[0].name.clone()),
        profile.name
    );
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "second Save starts no write"
    );
    drop(release);
    worker.join().expect("background worker finishes");
    cx.run_until_parked();
    assert!(window_is_closed(&window, &mut cx));
    assert_completed_save(
        SaveFormFixture {
            host,
            fixture,
            app_state,
            profile,
            secret_ref,
            window,
        },
        writes.load(Ordering::SeqCst),
        close,
        &cx,
    );
}

fn assert_completed_save(form: SaveFormFixture, writes: usize, close: bool, cx: &TestAppContext) {
    let SaveFormFixture {
        host,
        fixture,
        app_state,
        profile,
        secret_ref,
        ..
    } = form;
    let saved_name = app_state.read_with(cx, |state, _| state.profiles()[0].name.clone());
    if close {
        assert_eq!(saved_name, profile.name, "refused save keeps the row");
        let values = fixture.values.lock().expect("values lock");
        assert_eq!(
            values.get(&secret_ref).map(|value| value.expose_secret()),
            Some("old token")
        );
        assert_eq!(
            host.read_with(cx, |host, _| host.toast_count()),
            1,
            "closed-window failure reported once"
        );
    } else {
        assert_eq!(saved_name, "saved once");
        assert_eq!(writes, 1, "profile saved once");
        assert_eq!(host.read_with(cx, |host, _| host.toast_count()), 0);
    }
}

#[::core::prelude::v1::test]
fn a_blocked_save_keeps_the_form_responsive_and_ignores_second_save() {
    run_blocked_save(false);
}

#[::core::prelude::v1::test]
fn a_closed_window_save_restores_secret_fields_and_reports_once() {
    run_blocked_save(true);
}

#[::core::prelude::v1::test]
fn the_saving_button_is_disabled_and_announces_saving() {
    use datatab_components::accessibility::{accessibility_node, accessible_name};
    use datatab_ui_base::keyboard_coverage::FrameCapture;
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let app_state = test_app_state(
        &mut cx,
        SecretStoreFixture::new(PasswordSaveOutcome::Success),
    );
    let window = open_new_profile_window(app_state, &mut cx);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), &cx);
    let capture = FrameCapture::observe(&mut visual);
    let idle = capture.frame(&mut visual);
    assert!(
        !accessibility_node(&idle, "save-connection")
            .expect("Save button")
            .is_disabled()
    );
    window
        .update(&mut cx, |manager, _, cx| {
            manager.saving = true;
            cx.notify();
        })
        .expect("form is open");
    let saving = capture.frame(&mut visual);
    assert!(
        accessibility_node(&saving, "save-connection")
            .expect("Save button")
            .is_disabled()
    );
    assert!(
        accessible_name(&saving, "save-connection")
            .expect("Save button name")
            .starts_with(&datatab_i18n::t!("form.action.saving"))
    );
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close form");
}

fn run_deleted_profile_save(close: bool) {
    let mut cx = TestAppContext::single();
    let form = SaveFormFixture::new(&mut cx, false);
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let writes = Arc::new(AtomicUsize::new(0));
    let removing_profile = Arc::new(AtomicBool::new(false));
    let (entered, receiver) = mpsc::channel();
    let dispatcher = cx.dispatcher.clone();
    form.app_state.update(&mut cx, |state, _| {
        *state.secret_store().write().expect("secret store lock") = Box::new(BlockingWrites {
            fixture: form.fixture.clone(),
            foreground: std::thread::current().id(),
            dispatcher,
            released: released.clone(),
            entered,
            writes: writes.clone(),
            removing_profile: removing_profile.clone(),
        });
    });
    form.window
        .update(&mut cx, |manager, window, cx| {
            manager
                .form
                .driver_inputs
                .get("api_token")
                .expect("token input")
                .update(cx, |input, cx| input.set_value("new token", window, cx));
            manager.form.form_save_password = true;
            manager
                .form
                .input_password
                .update(cx, |input, cx| input.set_value("primary", window, cx));
            manager.form.form_save_ssh_secret = true;
            manager.access.ssh_enabled = true;
            manager.access.input_ssh_host.update(cx, |input, cx| {
                input.set_value("bastion.example", window, cx)
            });
            manager
                .access
                .input_ssh_user
                .update(cx, |input, cx| input.set_value("tunnel", window, cx));
            manager.access.ssh_auth_method = crate::ssh_shared::SshAuthSelection::Password;
            manager
                .access
                .input_ssh_password
                .update(cx, |input, cx| input.set_value("ssh secret", window, cx));
            manager.save_profile(window, cx);
        })
        .expect("save returns while keyring is blocked");
    let dispatcher = cx.dispatcher.clone();
    let worker = std::thread::spawn(move || while dispatcher.tick(true) {});
    let release = ReleaseWrites(released);
    receiver.recv().expect("background write reached the store");
    // Profile removal has its own foreground deletes; only save cleanup must be background.
    removing_profile.store(true, Ordering::SeqCst);
    form.app_state.update(&mut cx, |state, _| {
        assert_eq!(
            state.remove_profile(0).expect("profile removed").id,
            form.profile.id
        );
    });
    removing_profile.store(false, Ordering::SeqCst);
    if close {
        form.window
            .update(&mut cx, |_, window, _| window.remove_window())
            .expect("close saving window");
    }
    drop(release);
    worker.join().expect("background worker finishes");
    cx.run_until_parked();
    assert_eq!(
        writes.load(Ordering::SeqCst),
        3,
        "all three secret writes completed"
    );
    assert_deleted_save(form, close, &mut cx);
}

fn assert_deleted_save(form: SaveFormFixture, close: bool, cx: &mut TestAppContext) {
    form.app_state.read_with(cx, |state, _| {
        assert!(state.profiles().is_empty());
        assert!(
            state
                .storage_runtime()
                .connection_profiles()
                .all()
                .expect("stored rows")
                .is_empty()
        );
    });
    assert!(
        form.fixture
            .values
            .lock()
            .expect("keyring values")
            .is_empty(),
        "deleted profile has no keyring entries after save completes"
    );
    if close {
        assert_eq!(
            form.host.read_with(cx, |host, _| host.toast_count()),
            1,
            "closed-window deletion reported once"
        );
        assert_eq!(
            form.host
                .read_with(cx, |host, _| host.last_toast_occurrences()),
            Some(1)
        );
    } else {
        form.window
            .update(cx, |manager, window, _| {
                assert!(!manager.saving);
                assert_eq!(
                    manager.validation_errors,
                    vec![datatab_i18n::t!(
                        "form.validation.connection_deleted_while_saving"
                    )]
                );
                window.remove_window();
            })
            .expect("deleted connection failure keeps the form open");
        assert_eq!(
            form.host.read_with(cx, |host, _| host.toast_count()),
            0,
            "inline deletion report has no duplicate toast"
        );
    }
}

#[::core::prelude::v1::test]
fn a_deleted_profile_save_removes_secrets_and_reports_inline_once() {
    run_deleted_profile_save(false);
}

#[::core::prelude::v1::test]
fn a_deleted_profile_save_after_window_close_removes_secrets_and_reports_once() {
    run_deleted_profile_save(true);
}
