//! Persisted update preferences and first-run bookkeeping.

use datatab_storage::bootstrap::StorageRuntime;
use datatab_storage::error::StorageError;
use datatab_storage::repositories::update_settings::UpdateSettingsDto;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSettings {
    /// Open the "What is new" dialog on the first start of a newer version.
    pub show_whats_new_after_update: bool,
    /// Version of the previous start, `None` before the first one.
    pub last_run_version: Option<String>,
    /// Whether the first-run welcome dialog was already shown.
    pub welcome_shown: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            show_whats_new_after_update: true,
            last_run_version: None,
            welcome_shown: false,
        }
    }
}

impl UpdateSettings {
    /// The user preference restored to its default, keeping the first-run
    /// bookkeeping untouched.
    pub fn with_default_preferences(&self) -> Self {
        let defaults = Self::default();

        Self {
            show_whats_new_after_update: defaults.show_whats_new_after_update,
            ..self.clone()
        }
    }

    fn from_dto(dto: UpdateSettingsDto) -> Self {
        Self {
            show_whats_new_after_update: dto.show_whats_new_after_update != 0,
            last_run_version: dto.last_run_version.filter(|value| !value.is_empty()),
            welcome_shown: dto.welcome_shown != 0,
        }
    }

    fn to_dto(&self) -> UpdateSettingsDto {
        UpdateSettingsDto {
            show_whats_new_after_update: i32::from(self.show_whats_new_after_update),
            last_run_version: self.last_run_version.clone(),
            welcome_shown: i32::from(self.welcome_shown),
        }
    }
}

/// Loads the update settings, falling back to the defaults when the row is
/// missing or unreadable.
pub fn load_update_settings(runtime: &StorageRuntime) -> UpdateSettings {
    match runtime.update_settings().get() {
        Ok(Some(dto)) => UpdateSettings::from_dto(dto),
        Ok(None) => UpdateSettings::default(),
        Err(error) => {
            log::warn!("Failed to load update settings, using defaults: {error}");
            UpdateSettings::default()
        }
    }
}

pub fn save_update_settings(
    runtime: &StorageRuntime,
    settings: &UpdateSettings,
) -> Result<(), StorageError> {
    runtime.update_settings().upsert(&settings.to_dto())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_loads_the_defaults() {
        let runtime = StorageRuntime::in_memory().expect("in-memory storage");

        assert_eq!(load_update_settings(&runtime), UpdateSettings::default());
    }

    #[test]
    fn settings_round_trip_through_storage() {
        let runtime = StorageRuntime::in_memory().expect("in-memory storage");
        let written = UpdateSettings {
            show_whats_new_after_update: false,
            last_run_version: Some("0.8.0".to_string()),
            welcome_shown: true,
        };

        save_update_settings(&runtime, &written).expect("save");

        assert_eq!(load_update_settings(&runtime), written);
    }

    #[test]
    fn default_preferences_keep_the_bookkeeping() {
        let customized = UpdateSettings {
            show_whats_new_after_update: false,
            last_run_version: Some("0.8.0".to_string()),
            welcome_shown: true,
        };

        let reset = customized.with_default_preferences();

        assert!(reset.show_whats_new_after_update);
        assert_eq!(reset.last_run_version.as_deref(), Some("0.8.0"));
        assert!(reset.welcome_shown);
    }
}
