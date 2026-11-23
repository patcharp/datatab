//! `datatab_app` — Pure domain modules for DataTab.
//!
//! This crate contains modules with no GPUI dependency, making them usable
//! from both the main application and external tools/servers.

pub mod access_manager;
pub mod app_state;
pub mod auth_provider_registry;
pub mod config_loader;
pub mod history_manager_sqlite;
pub mod hook_executor;
pub mod keymap;
pub mod mcp_command;
pub mod portability;
pub mod proxy;
pub mod rpc_services;
pub mod secret_provider_registry;
pub mod updates;

pub use access_manager::AppAccessManager;
pub use app_state::{
    AppState, ConnectionPasswordPrompt, SecretFieldChanges, SecretFieldKeyringError,
    SecretFieldsMayHaveChanged,
};
pub use auth_provider_registry::{AuthProviderRegistry, RegistryAuthProviderWrapper};
pub use hook_executor::CompositeExecutor;
pub use rpc_services::{ExternalDriverDiagnostic, ExternalDriverStage};
pub use secret_provider_registry::SecretProviderRegistry;
