use interprocess::local_socket::{GenericNamespaced, Name, ToNsName};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static APP_CONTROL_NAME: OnceLock<String> = OnceLock::new();

fn app_control_name_for_data_dir(data_dir: &Path, debug: bool) -> io::Result<String> {
    let canonical = data_dir.canonicalize()?;
    let hash = stable_path_hash(&canonical);
    let suffix = if debug { "-debug" } else { "" };
    Ok(format!("datatab-{hash:016x}{suffix}.sock"))
}

fn stable_path_hash(path: &Path) -> u64 {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        fnv1a(path.as_os_str().encode_wide().flat_map(u16::to_le_bytes))
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        fnv1a(path.as_os_str().as_bytes().iter().copied())
    }
}

fn fnv1a(bytes: impl Iterator<Item = u8>) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    bytes.fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

fn app_control_name(data_dir: io::Result<PathBuf>, debug: bool) -> String {
    match data_dir.and_then(|directory| app_control_name_for_data_dir(&directory, debug)) {
        Ok(name) => name,
        Err(_) => {
            let suffix = if debug { "-debug" } else { "" };
            let fallback = format!("datatab{suffix}.sock");
            // Startup and CLI calls can precede logger initialization.
            eprintln!("Could not resolve app-control data folder; using {fallback}");
            fallback
        }
    }
}

/// Returns the local socket name for the main DataTab app-control channel.
///
/// Names are scoped to the canonical data folder. Debug and release builds
/// use distinct names so both can run simultaneously.
/// The underlying transport is platform-specific:
/// - Linux: abstract namespace Unix domain socket
/// - macOS: Unix domain socket in `/tmp/`
/// - Windows: named pipe
pub fn socket_name() -> io::Result<Name<'static>> {
    APP_CONTROL_NAME
        .get_or_init(|| {
            let data_dir = datatab_storage::paths::data_dir()
                .map_err(|error| io::Error::other(error.to_string()));
            app_control_name(data_dir, cfg!(debug_assertions))
        })
        .clone()
        .to_ns_name::<GenericNamespaced>()
}

/// Returns the local socket name for an IPC driver-host process.
///
/// Each driver-host gets a unique socket name based on its identifier.
pub fn driver_socket_name(id: &str) -> io::Result<Name<'static>> {
    let suffix = if cfg!(debug_assertions) { "-debug" } else { "" };
    format!("datatab-driver-{id}{suffix}.sock").to_ns_name::<GenericNamespaced>()
}

pub fn auth_provider_socket_name(id: &str) -> io::Result<Name<'static>> {
    let suffix = if cfg!(debug_assertions) { "-debug" } else { "" };
    format!("datatab-auth-provider-{id}{suffix}.sock").to_ns_name::<GenericNamespaced>()
}

pub fn secret_provider_socket_name(id: &str) -> io::Result<Name<'static>> {
    let suffix = if cfg!(debug_assertions) { "-debug" } else { "" };
    format!("datatab-secret-provider-{id}{suffix}.sock").to_ns_name::<GenericNamespaced>()
}

#[cfg(test)]
mod tests;
