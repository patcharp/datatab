#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use clap::error::ErrorKind;

#[test]
fn help_and_version_exit_successfully() {
    for flag in ["--help", "-h", "--version", "-V"] {
        let error = parse(["datatab", flag]).expect_err("informational flag must exit");
        assert_eq!(error.exit_code(), 0);
        let output = error.to_string();
        if flag.contains('h') {
            assert!(output.contains("Usage: datatab"));
        } else {
            assert!(output.contains(env!("CARGO_PKG_VERSION")));
        }
    }
}

#[test]
fn invalid_arguments_never_reach_dispatch() {
    for (argument, kind) in [
        ("--bogus", ErrorKind::UnknownArgument),
        ("mpc", ErrorKind::ValueValidation),
        ("missing-script-255.sql", ErrorKind::ValueValidation),
    ] {
        let error = parse(["datatab", argument]).expect_err("invalid input must not dispatch");
        assert_ne!(error.exit_code(), 0);
        assert_eq!(error.kind(), kind);
        if kind == ErrorKind::ValueValidation {
            assert!(error.to_string().contains("scripts are not existing files"));
            assert!(error.to_string().contains(argument));
        }
    }
}

#[test]
fn mcp_accepts_all_supported_options() {
    let cli = parse([
        "datatab",
        "mcp",
        "--client-id",
        "client",
        "--config-dir",
        "/tmp/config",
    ])
    .expect("MCP arguments must parse");
    match cli.run() {
        Dispatch::Mcp {
            client_id,
            config_dir,
        } => {
            assert_eq!(client_id, "client");
            assert_eq!(config_dir, Some(PathBuf::from("/tmp/config")));
        }
        _ => panic!("expected MCP dispatch"),
    }
}

#[test]
fn two_existing_files_are_accepted() {
    let file = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    parse(["datatab", file, file]).expect("multiple existing files must parse");
}

#[test]
fn gui_flag_dispatches_gui() {
    assert!(matches!(
        parse(["datatab", "--gui"]).expect("gui flag").run(),
        Dispatch::RunGui
    ));
}

#[test]
fn existing_file_is_made_absolute() {
    let cli = parse(["datatab", "Cargo.toml"]).expect("existing file");
    assert_eq!(
        cli.path,
        vec![std::path::absolute("Cargo.toml").expect("absolute path")]
    );
}

#[test]
fn multiple_files_send_requests_in_order() {
    let cli = parse(["datatab", "Cargo.toml", "src/lib.rs"]).expect("existing files");
    let sent = std::cell::RefCell::new(Vec::new());
    launcher::open_with(
        &cli.path,
        |path| {
            sent.borrow_mut().push(path.clone());
            Ok(())
        },
        || panic!("must not spawn"),
        |_| panic!("must not retry"),
    )
    .expect("requests succeed");
    assert_eq!(
        *sent.borrow(),
        vec![
            std::path::absolute("Cargo.toml").expect("absolute"),
            std::path::absolute("src/lib.rs").expect("absolute")
        ]
    );
}

#[test]
fn invalid_paths_prevent_all_requests() {
    let sent = std::cell::RefCell::new(Vec::new());
    let parsed = parse([
        "datatab",
        "Cargo.toml",
        "missing-255-a.sql",
        "missing-255-b.sql",
    ]);
    if let Ok(cli) = &parsed {
        launcher::open_with(
            &cli.path,
            |path| {
                sent.borrow_mut().push(path.clone());
                Ok(())
            },
            || panic!("must not spawn"),
            |_| panic!("must not retry"),
        )
        .expect("send");
    }
    let error = parsed.expect_err("missing files must fail validation");
    assert_eq!(error.kind(), ErrorKind::ValueValidation);
    assert!(error.to_string().contains("missing-255-a.sql"));
    assert!(error.to_string().contains("missing-255-b.sql"));
    assert!(sent.borrow().is_empty());
}

#[test]
fn mcp_requires_client_id() {
    let error = parse(["datatab", "mcp"]).expect_err("client id is required");
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn starts_once_then_retries_first_before_remaining_files() {
    let paths = vec![PathBuf::from("first"), PathBuf::from("second")];
    let actions = std::cell::RefCell::new(Vec::new());
    let first_attempt = std::cell::Cell::new(true);
    launcher::open_with(
        &paths,
        |path| {
            actions
                .borrow_mut()
                .push(format!("send {}", path.display()));
            if path == &paths[0] && first_attempt.replace(false) {
                Err(datatab_ipc::AppControlError::Unavailable(
                    std::io::Error::other("not running"),
                ))
            } else {
                Ok(())
            }
        },
        || {
            actions.borrow_mut().push("spawn".into());
            Ok(())
        },
        |path| {
            actions
                .borrow_mut()
                .push(format!("retry {}", path.display()));
            Ok(())
        },
    )
    .expect("launch succeeds");
    assert_eq!(
        *actions.borrow(),
        [
            "send first",
            "spawn",
            "retry first",
            "send first",
            "send second"
        ]
    );
}

#[test]
fn running_instance_error_does_not_spawn() {
    let paths = vec![PathBuf::from("first"), PathBuf::from("second")];
    let error = launcher::open_with(
        &paths,
        |_| {
            Err(datatab_ipc::AppControlError::Request(
                std::io::Error::other("rejected"),
            ))
        },
        || panic!("must not spawn"),
        |_| panic!("must not retry"),
    )
    .expect_err("request error must be returned");
    assert_eq!(error.to_string(), "rejected");
}

#[test]
fn unresponsive_instance_does_not_spawn() {
    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, ToNsName, traits::Listener as _,
    };
    use std::{thread, time::Duration};
    let name = format!("datatab-253-launcher-{}", std::process::id())
        .to_ns_name::<GenericNamespaced>()
        .expect("name");
    let listener = ListenerOptions::new()
        .name(name.clone())
        .create_sync()
        .expect("listener");
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let _stream = listener.accept().expect("accept");
        wait.recv().expect("release");
    });
    let error = launcher::open_with(
        &[PathBuf::from("first")],
        |_| {
            datatab_ipc::send_test_request(
                name.clone(),
                datatab_ipc::IpcMessage::Focus,
                Duration::from_secs(1),
            )
        },
        || panic!("must not spawn"),
        |_| panic!("must not retry"),
    )
    .expect_err("timeout must be returned");
    if release.send(()).is_err() {
        panic!("server closed before timeout");
    }
    server.join().expect("server");
    assert_eq!(error.to_string(), "DataTab is running but not responding");
}

#[test]
fn focus_timeout_exits_without_starting_gui() {
    let error = std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "DataTab is running but not responding",
    );
    assert!(matches!(
        dispatch_focus(Err(datatab_ipc::AppControlError::Request(error))),
        Dispatch::Exit(1)
    ));
    let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
    assert!(matches!(
        dispatch_focus(Err(datatab_ipc::AppControlError::Unavailable(missing))),
        Dispatch::RunGui
    ));
}

#[test]
fn post_spawn_timeout_retries() {
    let attempts = std::cell::Cell::new(0);
    let first_attempt = std::cell::Cell::new(true);
    launcher::open_with(
        &[PathBuf::from("first")],
        |_| {
            if !first_attempt.replace(false) {
                return Ok(());
            }
            Err(datatab_ipc::AppControlError::Unavailable(
                std::io::Error::other("missing"),
            ))
        },
        || Ok(()),
        |_| {
            launcher::retry_with(|| {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Err(datatab_ipc::AppControlError::Request(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timeout",
                    )))
                } else {
                    Ok(())
                }
            })
        },
    )
    .expect("retry succeeds");
    assert_eq!(attempts.get(), 2);
}

#[test]
fn post_spawn_timeout_reaches_startup_cap() {
    let error = launcher::open_with(
        &[PathBuf::from("first")],
        |_| {
            Err(datatab_ipc::AppControlError::Unavailable(
                std::io::Error::other("missing"),
            ))
        },
        || Ok(()),
        |_| {
            launcher::retry_with_timeout(
                |_| {
                    Err(datatab_ipc::AppControlError::Request(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timeout",
                    )))
                },
                std::time::Duration::from_millis(120),
            )
        },
    )
    .expect_err("startup deadline");
    assert_eq!(error.to_string(), "DataTab did not start in time");
}

#[test]
fn delayed_ready_server_receives_each_script_once() {
    use datatab_ipc::{
        AppControlError, IpcMessage,
        protocol::{AppControlRequest, AppControlResponse, IpcResponse},
    };
    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, ToNsName, traits::Listener as _,
    };
    use std::{sync::mpsc, thread, time::Duration};

    let name = format!("datatab-253-ready-{}", std::process::id())
        .to_ns_name::<GenericNamespaced>()
        .expect("name");
    let listener = ListenerOptions::new()
        .name(name.clone())
        .create_sync()
        .expect("listener");
    let (ping_seen, wait_ping) = mpsc::channel();
    let (release, wait_release) = mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let mut paths = Vec::new();
        loop {
            let mut stream = listener.accept().expect("accept");
            let request: AppControlRequest = datatab_ipc::recv_msg(&mut stream).expect("request");
            let response = match request.body {
                IpcMessage::Ping => {
                    ping_seen.send(()).expect("ping observed");
                    wait_release.recv().expect("release ping");
                    IpcResponse::Pong {
                        version: "test".into(),
                    }
                }
                IpcMessage::OpenScript { path } => {
                    paths.push(path);
                    IpcResponse::Ok
                }
                IpcMessage::Focus => break,
            };
            datatab_ipc::send_msg(
                &mut stream,
                &AppControlResponse::ok(request.request_id, response),
            )
            .expect("response");
        }
        paths
    });
    let client_name = name.clone();
    let client = thread::spawn(move || {
        let mut first = true;
        let result = launcher::open_with(
            &[PathBuf::from("first"), PathBuf::from("second")],
            |path| {
                if first {
                    first = false;
                    return Err(AppControlError::Unavailable(std::io::Error::from(
                        std::io::ErrorKind::NotFound,
                    )));
                }
                datatab_ipc::send_test_request(
                    client_name.clone(),
                    IpcMessage::OpenScript { path: path.clone() },
                    Duration::from_secs(5),
                )
            },
            || Ok(()),
            |_| {
                launcher::retry_with_timeout(
                    |timeout| {
                        datatab_ipc::send_test_request(
                            client_name.clone(),
                            IpcMessage::Ping,
                            timeout.min(Duration::from_secs(5)),
                        )
                    },
                    Duration::from_secs(10),
                )
            },
        );
        // The sentinel releases the server after the launcher has finished.
        let _result =
            datatab_ipc::send_test_request(client_name, IpcMessage::Focus, Duration::from_secs(1));
        result
    });
    wait_ping.recv().expect("ping received");
    release.send(()).expect("release ping");
    client.join().expect("client").expect("launch");
    assert_eq!(
        server.join().expect("server"),
        [PathBuf::from("first"), PathBuf::from("second")]
    );
}
