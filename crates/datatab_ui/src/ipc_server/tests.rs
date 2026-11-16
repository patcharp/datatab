#![allow(clippy::expect_used)]
use super::{AppControlRequest, AppControlResponse, IpcMessage, IpcResponse, spawn_connection};
use datatab_ipc::framing;
use interprocess::local_socket::Stream as IpcStream;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName, prelude::*};
use std::{io, sync::mpsc, thread, time::Duration};

#[test]
fn canonicalized_script_spellings_share_a_dedup_key() {
    use std::path::Path;

    let directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let key = |path: std::path::PathBuf| {
        datatab_ui_document::simplify_path(&path.canonicalize().expect("existing file"))
    };
    assert_eq!(
        key(directory.join("./Cargo.toml")),
        key(directory.join("Cargo.toml"))
    );
}

#[test]
fn silent_connection_drops_and_next_is_served() {
    let name = format!("datatab-253-server-{}", std::process::id())
        .to_ns_name::<GenericNamespaced>()
        .expect("name");
    let listener = ListenerOptions::new()
        .name(name.clone())
        .create_sync()
        .expect("listener");
    let (tx, _rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let silent = listener.accept().expect("first");
        let first = spawn_connection(silent, tx.clone(), "token".into(), Duration::from_secs(1));
        let first = first.expect("spawn first");
        let next = listener.accept().expect("second");
        let second = spawn_connection(next, tx, "token".into(), Duration::from_secs(1));
        second
            .expect("spawn second")
            .join()
            .expect("second thread")
            .expect("second served");
        let error = first.join().expect("first").expect_err("silent dropped");
        #[cfg(unix)]
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        #[cfg(windows)]
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    });
    let silent = IpcStream::connect(name.clone()).expect("silent connection");
    let (release, wait) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        let _stream = silent;
        wait.recv().expect("release");
    });
    let mut next = IpcStream::connect(name).expect("second connection");
    framing::send_msg(
        &mut next,
        &AppControlRequest::new(1, Some("token".into()), IpcMessage::Ping),
    )
    .expect("send");
    let response: AppControlResponse = framing::recv_msg(&mut next).expect("response");
    assert!(matches!(response.body, IpcResponse::Pong { .. }));
    #[cfg(windows)]
    release.send(()).expect("release silent client");
    let result = server.join();
    #[cfg(unix)]
    if release.send(()).is_err() {
        eprintln!("silent connection already closed");
    }
    holder.join().expect("holder");
    result.expect("server");
}
