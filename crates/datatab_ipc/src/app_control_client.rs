use crate::protocol::{AppControlRequest, AppControlResponse, IpcMessage, IpcResponse};
use crate::{APP_CONTROL_VERSION, framing, read_app_control_token, socket_name};
use interprocess::{
    ConnectWaitMode,
    local_socket::{ConnectOptions, Stream},
};
use std::io;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

pub const APP_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub enum AppControlError {
    Unavailable(io::Error),
    Request(io::Error),
}

pub fn send_app_control_request(msg: IpcMessage) -> io::Result<()> {
    send_app_control_request_checked(msg).map_err(|error| match error {
        AppControlError::Unavailable(error) | AppControlError::Request(error) => error,
    })
}

pub fn send_app_control_request_checked(msg: IpcMessage) -> Result<(), AppControlError> {
    send_app_control_request_with_timeout(msg, APP_CONTROL_TIMEOUT)
}

pub fn send_app_control_request_with_timeout(
    msg: IpcMessage,
    timeout: Duration,
) -> Result<(), AppControlError> {
    let name = socket_name().map_err(AppControlError::Request)?;
    send_to(name, msg, timeout, read_app_control_token)
}

fn send_to(
    name: interprocess::local_socket::Name<'_>,
    msg: IpcMessage,
    timeout: Duration,
    token: impl FnOnce() -> io::Result<String> + Send + 'static,
) -> Result<(), AppControlError> {
    let name = name.into_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("app-control-client".into())
        .spawn(move || {
            let result = send_request(name, msg, timeout, token);
            if sender.send(result).is_err() {
                log::debug!("App-control caller stopped waiting");
            }
        })
        .map_err(AppControlError::Request)?;
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AppControlError::Request(not_responding())),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(AppControlError::Request(
            io::Error::other("app-control worker stopped"),
        )),
    }
}

fn send_request(
    name: interprocess::local_socket::Name<'_>,
    msg: IpcMessage,
    timeout: Duration,
    token: impl FnOnce() -> io::Result<String>,
) -> Result<(), AppControlError> {
    let mut stream = ConnectOptions::new()
        .name(name)
        .wait_mode(ConnectWaitMode::Timeout(timeout))
        .connect_sync()
        .map_err(classify_connect_error)?;
    send_connected(&mut stream, msg, token().map_err(AppControlError::Request)?).map_err(|error| {
        AppControlError::Request(if error.kind() == io::ErrorKind::TimedOut {
            not_responding()
        } else {
            error
        })
    })
}

fn classify_connect_error(error: io::Error) -> AppControlError {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            AppControlError::Unavailable(error)
        }
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            AppControlError::Request(not_responding())
        }
        _ => AppControlError::Request(error),
    }
}

fn not_responding() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "DataTab is running but not responding",
    )
}

fn send_connected(stream: &mut Stream, msg: IpcMessage, auth_token: String) -> io::Result<()> {
    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let is_ping = matches!(msg, IpcMessage::Ping);
    let request = AppControlRequest::new(request_id, Some(auth_token), msg);

    framing::send_msg(&mut *stream, &request)?;
    let response: AppControlResponse = framing::recv_msg(stream)?;

    if !response
        .protocol_version
        .is_compatible_with(APP_CONTROL_VERSION)
    {
        return Err(io::Error::other(
            "incompatible app-control protocol version",
        ));
    }

    if response.request_id != request_id {
        return Err(io::Error::other("mismatched app-control response id"));
    }

    match response.body {
        IpcResponse::Error { message } => Err(io::Error::other(message)),
        IpcResponse::Pong { .. } if is_ping => Ok(()),
        _ if is_ping => Err(io::Error::other("expected app-control pong")),
        _ => Ok(()),
    }
}

pub fn send_focus_request() -> io::Result<()> {
    send_app_control_request(IpcMessage::Focus)
}

#[cfg(feature = "test-support")]
pub fn send_test_request(
    name: interprocess::local_socket::Name<'_>,
    msg: IpcMessage,
    timeout: Duration,
) -> Result<(), AppControlError> {
    send_to(name, msg, timeout, || Ok("test-token".into()))
}

#[cfg(test)]
mod tests;
