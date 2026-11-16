use crate::ui::views::workspace::Workspace;
use datatab_ipc::{
    APP_CONTROL_VERSION, framing,
    protocol::{AppControlRequest, AppControlResponse, IpcMessage, IpcResponse},
};
use gpui::*;
#[cfg(unix)]
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{
    Listener as IpcListener, Stream as IpcStream, traits::Listener as _,
};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

const SERVER_TIMEOUT: Duration = Duration::from_secs(5);

pub struct IpcServer;

enum IpcCommand {
    OpenScript { path: PathBuf },
    Focus,
}

impl IpcServer {
    pub fn start_with_listener(
        listener: IpcListener,
        workspace: Entity<Workspace>,
        window: AnyWindowHandle,
        auth_token: String,
        cx: &mut App,
    ) {
        let (cmd_tx, cmd_rx): (Sender<IpcCommand>, Receiver<IpcCommand>) = mpsc::channel();

        thread::spawn(move || {
            accept_loop(listener, cmd_tx, auth_token);
        });

        cx.spawn(async move |cx| {
            process_commands(cmd_rx, workspace, window, cx.clone()).await;
        })
        .detach();
    }
}

fn accept_loop(listener: IpcListener, cmd_tx: Sender<IpcCommand>, auth_token: String) {
    loop {
        match listener.accept() {
            Ok(stream) => {
                if let Err(error) =
                    spawn_connection(stream, cmd_tx.clone(), auth_token.clone(), SERVER_TIMEOUT)
                {
                    log::warn!("IPC connection thread failed: {}", error);
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                log::warn!("IPC accept error: {}", e);
                break;
            }
        }
    }
}

fn spawn_connection(
    stream: IpcStream,
    cmd_tx: Sender<IpcCommand>,
    auth_token: String,
    timeout: Duration,
) -> io::Result<thread::JoinHandle<io::Result<()>>> {
    thread::Builder::new()
        .name("app-control-server".into())
        .spawn(move || {
            let result = handle_connection(stream, &cmd_tx, &auth_token, timeout);
            if let Err(error) = &result {
                log::warn!("IPC connection error: {:?}", error.kind());
            }
            result
        })
}

fn handle_connection(
    mut stream: IpcStream,
    cmd_tx: &Sender<IpcCommand>,
    expected_auth_token: &str,
    timeout: Duration,
) -> io::Result<()> {
    #[cfg(unix)]
    stream.set_recv_timeout(Some(timeout))?;
    #[cfg(not(unix))]
    let _timeout = timeout;
    let request: AppControlRequest = framing::recv_msg(&mut stream)?;
    let request_id = request.request_id;

    if !request
        .protocol_version
        .is_compatible_with(APP_CONTROL_VERSION)
    {
        let response = AppControlResponse::ok(
            request_id,
            IpcResponse::Error {
                message: "incompatible app-control protocol version".to_string(),
            },
        );
        framing::send_msg(&mut stream, &response)?;
        return Ok(());
    }

    if request.auth_token.as_deref() != Some(expected_auth_token) {
        let response = AppControlResponse::ok(
            request_id,
            IpcResponse::Error {
                message: "unauthorized app-control request".to_string(),
            },
        );
        framing::send_msg(&mut stream, &response)?;
        return Ok(());
    }

    let response_body = match request.body {
        IpcMessage::Ping => IpcResponse::Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        IpcMessage::OpenScript { path } => {
            let path = path.canonicalize().unwrap_or(path);
            let path = datatab_ui_document::simplify_path(&path);
            if cmd_tx.send(IpcCommand::OpenScript { path }).is_ok() {
                IpcResponse::Ok
            } else {
                IpcResponse::Error {
                    message: "failed to send command".into(),
                }
            }
        }
        IpcMessage::Focus => {
            if cmd_tx.send(IpcCommand::Focus).is_ok() {
                IpcResponse::Ok
            } else {
                IpcResponse::Error {
                    message: "failed to send command".into(),
                }
            }
        }
    };

    let response = AppControlResponse::ok(request_id, response_body);

    framing::send_msg(&mut stream, &response)?;
    Ok(())
}

async fn process_commands(
    cmd_rx: Receiver<IpcCommand>,
    workspace: Entity<Workspace>,
    window: AnyWindowHandle,
    cx: AsyncApp,
) {
    loop {
        match cmd_rx.try_recv() {
            Ok(cmd) => {
                cx.update(|cx| match cmd {
                    IpcCommand::OpenScript { path } => {
                        workspace.update(cx, |ws, cx| {
                            ws.open_script_from_path(path, cx);
                        });
                    }
                    IpcCommand::Focus => {
                        if let Err(e) = window.update(cx, |_root, window, _cx| {
                            window.activate_window();
                        }) {
                            log::warn!("Failed to focus main window via IPC: {:?}", e);
                        }
                    }
                });
            }
            Err(mpsc::TryRecvError::Empty) => {
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests;
