#![allow(clippy::expect_used, clippy::panic)]
use super::*;
use interprocess::local_socket::{
    GenericNamespaced, ListenerOptions, ToNsName, traits::Listener as _,
};

#[test]
fn silent_server_returns_not_responding() {
    let name = format!("datatab-253-client-{}", std::process::id())
        .to_ns_name::<GenericNamespaced>()
        .expect("name");
    let listener = ListenerOptions::new()
        .name(name.clone())
        .create_sync()
        .expect("listener");
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let _stream = listener.accept().expect("accept");
        wait.recv().expect("release");
    });
    let result = send_to(name, IpcMessage::Focus, Duration::from_secs(1), || {
        Ok("token".into())
    });
    if release.send(()).is_err() {
        server.join().expect("server");
        panic!("server closed before timeout");
    }
    server.join().expect("server");
    match result {
        Err(AppControlError::Request(error)) => {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(error.to_string(), "DataTab is running but not responding");
        }
        _ => panic!("expected timeout"),
    }
}

#[test]
fn would_block_connect_reports_not_responding() {
    let error = classify_connect_error(io::Error::from(io::ErrorKind::WouldBlock));
    match error {
        AppControlError::Request(error) => {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(error.to_string(), "DataTab is running but not responding");
        }
        _ => panic!("expected request error"),
    }
}

#[test]
fn blocked_request_work_returns_without_waiting_for_worker() {
    let name = format!("datatab-253-worker-{}", std::process::id())
        .to_ns_name::<GenericNamespaced>()
        .expect("name");
    let listener = ListenerOptions::new()
        .name(name.clone())
        .create_sync()
        .expect("listener");
    let (release_token, wait_token) = std::sync::mpsc::channel::<()>();
    let (release_server, wait_server) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let _stream = listener.accept().expect("accept");
        wait_server.recv().expect("release server");
    });
    let (result_sender, result_receiver) = std::sync::mpsc::channel();
    let caller = std::thread::spawn(move || {
        let result = send_to(name, IpcMessage::Focus, Duration::from_secs(1), move || {
            wait_token.recv().expect("release token");
            Ok("token".into())
        });
        result_sender.send(result).expect("result receiver");
    });
    let result = result_receiver.recv_timeout(Duration::from_secs(3));
    release_token.send(()).expect("release token");
    release_server.send(()).expect("release server");
    caller.join().expect("caller");
    server.join().expect("server");
    let Err(AppControlError::Request(error)) = result.expect("request stayed blocked") else {
        panic!("expected request timeout");
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}
