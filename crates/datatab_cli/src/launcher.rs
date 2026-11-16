use datatab_ipc::{
    APP_CONTROL_TIMEOUT, AppControlError, protocol::IpcMessage, send_app_control_request_checked,
    send_app_control_request_with_timeout,
};
use std::{
    io,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_DELAY_MS: u64 = 50;

pub(crate) fn focus() -> Result<(), AppControlError> {
    send_app_control_request_checked(IpcMessage::Focus)
}

pub(crate) fn open(paths: &[PathBuf]) -> i32 {
    match open_with(
        paths,
        |path| send_app_control_request_checked(IpcMessage::OpenScript { path: path.clone() }),
        spawn_gui,
        |_| retry_send(),
    ) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Failed to open scripts: {error}");
            1
        }
    }
}

pub(crate) fn open_with(
    paths: &[PathBuf],
    mut send: impl FnMut(&PathBuf) -> Result<(), AppControlError>,
    mut spawn: impl FnMut() -> io::Result<()>,
    mut retry: impl FnMut(&PathBuf) -> io::Result<()>,
) -> io::Result<()> {
    let Some((first, remaining)) = paths.split_first() else {
        return Ok(());
    };
    match send(first) {
        Ok(()) => {}
        Err(AppControlError::Unavailable(_)) => {
            spawn()?;
            retry(first)?;
            send(first).map_err(|error| match error {
                AppControlError::Unavailable(error) | AppControlError::Request(error) => error,
            })?;
        }
        Err(AppControlError::Request(error)) => return Err(error),
    }
    for path in remaining {
        send(path).map_err(|error| match error {
            AppControlError::Unavailable(error) | AppControlError::Request(error) => error,
        })?;
    }
    Ok(())
}

fn retry_send() -> io::Result<()> {
    retry_with_timeout(
        |timeout| {
            send_app_control_request_with_timeout(
                IpcMessage::Ping,
                timeout.min(APP_CONTROL_TIMEOUT),
            )
        },
        STARTUP_TIMEOUT,
    )
}

#[cfg(test)]
pub(crate) fn retry_with(mut send: impl FnMut() -> Result<(), AppControlError>) -> io::Result<()> {
    retry_with_timeout(|_| send(), STARTUP_TIMEOUT)
}

pub(crate) fn retry_with_timeout(
    mut send: impl FnMut(Duration) -> Result<(), AppControlError>,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "DataTab did not start in time",
            ));
        }
        std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS).min(remaining));
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "DataTab did not start in time",
            ));
        }
        match send(remaining) {
            Ok(()) => return Ok(()),
            Err(AppControlError::Unavailable(_)) => {}
            Err(AppControlError::Request(error)) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(AppControlError::Request(error)) => return Err(error),
        }
    }
}

fn spawn_gui() -> io::Result<()> {
    let exe = std::env::current_exe()?;

    let mut cmd = Command::new(exe);
    cmd.arg("--gui")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    #[expect(
        unsafe_code,
        reason = "`CommandExt::pre_exec` is the documented unsafe API for \
                  post-fork, pre-exec setup; the closure only calls `setsid()` \
                  and maps `errno`, both async-signal-safe"
    )]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    cmd.spawn()?;
    Ok(())
}
