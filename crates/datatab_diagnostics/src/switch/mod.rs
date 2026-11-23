//! The off switch, honoured in one place.
//!
//! Three sources can turn reports off: the user setting, an environment
//! variable and a managed file an organization places on the machine. Any one
//! of them is enough; none of them can force reports on. The organization's
//! sources outrank the user's setting, because an organization that forbids
//! reports must not be overridden by a per-user toggle.
//!
//! ADR 0008 names the sources and leaves their exact names to the stage 1b
//! issue; the names below are this stage's choice and are what #467 documents.

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// Set to disable reports for every launch of this user or process.
pub const DISABLE_ENV_VAR: &str = "DATATAB_DISABLE_ERROR_REPORTS";

/// Name of the managed file an organization drops on the machine.
pub const DISABLE_FILE_NAME: &str = "error-reports-disabled";

/// Which source turned reports off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableSource {
    Setting,
    Environment,
    ManagedFile,
}

impl DisableSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setting => "setting",
            Self::Environment => "environment",
            Self::ManagedFile => "managed_file",
        }
    }
}

/// Whether a send action may appear, and why not when it may not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchState {
    Enabled,
    Disabled(DisableSource),
}

impl SwitchState {
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// The three sources, already read. Pure data so the decision is testable
/// without touching the environment or the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSwitch {
    pub setting_enabled: bool,
    pub env_value: Option<String>,
    pub managed_file_found: bool,
}

impl ReportSwitch {
    pub fn new(setting_enabled: bool, env_value: Option<String>, managed_file_found: bool) -> Self {
        Self {
            setting_enabled,
            env_value,
            managed_file_found,
        }
    }

    /// Reads the environment variable and looks for `managed_file_paths`.
    pub fn from_process(setting_enabled: bool, managed_file_paths: &[PathBuf]) -> Self {
        Self {
            setting_enabled,
            env_value: std::env::var(DISABLE_ENV_VAR).ok(),
            managed_file_found: managed_file_paths.iter().any(|path| path.is_file()),
        }
    }

    /// The decision. `ManagedFile` first, then `Environment`, then the setting.
    pub fn state(&self) -> SwitchState {
        if self.managed_file_found {
            return SwitchState::Disabled(DisableSource::ManagedFile);
        }

        if self.env_value.as_deref().is_some_and(env_value_disables) {
            return SwitchState::Disabled(DisableSource::Environment);
        }

        if !self.setting_enabled {
            return SwitchState::Disabled(DisableSource::Setting);
        }

        SwitchState::Enabled
    }

    /// The same decision as a boolean, for callers that only gate a button.
    pub fn is_enabled(&self) -> bool {
        self.state().is_enabled()
    }
}

/// Whether an environment value means "off".
///
/// An empty value and the usual false words leave reports alone; anything else
/// that is set counts as off, so a typo fails closed rather than open.
pub fn env_value_disables(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();

    !matches!(normalized.as_str(), "" | "0" | "false" | "no" | "off")
}

/// Where the managed file may be placed, most specific first.
///
/// The data directory holds the per-user file; the system paths are where an
/// organization's packaging puts a machine-wide one — `/etc/datatab` on Linux,
/// `Application Support` on macOS and `%ProgramData%\DataTab` on Windows.
pub fn managed_file_paths(data_dir: &Path) -> Vec<PathBuf> {
    managed_file_paths_with(data_dir, std::env::var_os(PROGRAM_DATA_ENV).as_deref())
}

/// `ProgramData` names the machine-wide application data directory on Windows.
const PROGRAM_DATA_ENV: &str = "ProgramData";

/// The same list with the Windows base directory passed in, so the Windows path
/// is covered by a test wherever the tests run.
fn managed_file_paths_with(
    data_dir: &Path,
    program_data: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let mut paths = vec![data_dir.join(DISABLE_FILE_NAME)];

    #[cfg(target_os = "linux")]
    paths.push(PathBuf::from("/etc/datatab").join(DISABLE_FILE_NAME));

    #[cfg(target_os = "macos")]
    paths.push(PathBuf::from("/Library/Application Support/DataTab").join(DISABLE_FILE_NAME));

    if let Some(base) = program_data {
        paths.push(Path::new(base).join("DataTab").join(DISABLE_FILE_NAME));
    }

    paths
}
