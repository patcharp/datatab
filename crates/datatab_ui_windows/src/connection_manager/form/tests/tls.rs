use super::*;
use crate::connection_manager::{MainExtraStop, SslCertSlot};

#[::core::prelude::v1::test]
fn tls_saved_redshift_verify_full_survives_edit_and_save() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let app_state = test_app_state(
        &mut cx,
        SecretStoreFixture::new(PasswordSaveOutcome::Success),
    );
    let mut config = DbConfig::default_redshift();
    if let DbConfig::Redshift { ssl_mode, .. } = &mut config {
        *ssl_mode = Some("verify-full".into());
    }
    let profile = ConnectionProfile::new("verified redshift", config);
    app_state.update(&mut cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None)
    });
    let window = open_edit_profile_window(app_state.clone(), profile, &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            assert_eq!(manager.form.selected_ssl_mode, "verify-full");
            manager.save_profile(window, cx);
        })
        .expect("save runs");
    let saved = cx.update(|cx| app_state.read(cx).profiles().to_vec());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "edited profile", "the edit was persisted");
    assert!(
        matches!(&saved[0].config, DbConfig::Redshift { ssl_mode: Some(mode), .. } if mode == "verify-full")
    );
    assert!(
        window.root(&mut cx).is_err(),
        "successful save closes the window"
    );
}

#[::core::prelude::v1::test]
fn tls_postgres_certificate_keyboard_stops_follow_mode() {
    let mut cx = TestAppContext::single();
    let window = open_postgres_form(&mut cx);
    for (mode, expected) in [
        ("disable", vec![]),
        (
            "prefer",
            vec![SslCertSlot::ClientCert, SslCertSlot::ClientKey],
        ),
        (
            "verify-full",
            vec![
                SslCertSlot::CaCert,
                SslCertSlot::ClientCert,
                SslCertSlot::ClientKey,
            ],
        ),
    ] {
        let slots = window
            .update(&mut cx, |manager, _, _| {
                manager.form.selected_ssl_mode = mode.into();
                manager
                    .main_extra_stops()
                    .into_iter()
                    .filter_map(|stop| match stop {
                        MainExtraStop::SslCert(slot) => Some(slot),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .expect("form remains open");
        assert_eq!(slots, expected, "mode {mode}");
    }
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close window");
}

#[::core::prelude::v1::test]
fn tls_postgres_rendered_certificate_pickers_follow_mode() {
    let mut cx = TestAppContext::single();
    let window = open_postgres_form(&mut cx);
    let capture = Arc::new(FrameCapture::default());
    for (mode, expected) in [
        ("disable", vec![]),
        (
            "prefer",
            vec!["ssl-cert-picker-client-cert", "ssl-cert-picker-client-key"],
        ),
        (
            "verify-full",
            vec![
                "ssl-cert-picker-ca",
                "ssl-cert-picker-client-cert",
                "ssl-cert-picker-client-key",
            ],
        ),
    ] {
        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.selected_ssl_mode = mode.into();
                window.observe_frames(&capture);
                window.refresh();
                cx.notify();
            })
            .expect("refresh form");
        cx.run_until_parked();
        let frame = capture
            .0
            .lock()
            .expect("frame lock")
            .clone()
            .expect("frame rendered");
        let mut pickers: Vec<_> = frame
            .nodes()
            .map(|(_, node)| node.id())
            .filter(|id| id.starts_with("ssl-cert-picker-"))
            .collect();
        pickers.sort_unstable();
        assert_eq!(pickers, expected, "mode {mode}");
    }
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close window");
}
