//! Where the two platform strings come from.
//!
//! `os` and `arch` are compile-time constants. `os_version` is read from the
//! platform's own description file, and is `None` where there is no file to
//! read: a report without a version is better than a guessed one.

/// The human name of this machine's OS release (`Ubuntu 24.04`, `macOS 14.5`).
///
/// Best effort and bounded: one small file read, no process, no network.
pub fn os_version() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux_version()
    }

    #[cfg(target_os = "macos")]
    {
        macos_version()
    }

    // Windows reports no version yet: the registry read needs Windows-only
    // code that this machine cannot compile or run (see the issue report).
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_version() -> Option<String> {
    let contents = std::fs::read_to_string("/etc/os-release").ok()?;

    for line in contents.lines() {
        let Some(value) = line.strip_prefix("PRETTY_NAME=") else {
            continue;
        };
        let trimmed = value.trim().trim_matches('"').trim();

        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn macos_version() -> Option<String> {
    const PLIST: &str = "/System/Library/CoreServices/SystemVersion.plist";
    const KEY: &str = "<key>ProductVersion</key>";

    let contents = std::fs::read_to_string(PLIST).ok()?;
    let after_key = contents.split_once(KEY)?.1;
    let value = after_key.split_once("<string>")?.1;
    let version = value.split_once("</string>")?.0.trim();

    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}
