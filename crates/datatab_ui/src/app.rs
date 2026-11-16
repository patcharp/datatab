//! Compatibility module re-exporting types from datatab_app.
//!
//! This module exists to ease the transition of UI code that previously
//! used `crate::app::AppState` when it was in the datatab crate.
//! New code should use `datatab_app::AppState` directly or `AppStateEntity`
//! from the parent crate.

pub use datatab_app::AppState;
pub use datatab_app::{ExternalDriverDiagnostic, ExternalDriverStage};
pub use datatab_core::ConnectedProfile;

// Re-export event types from datatab_ui_base
#[cfg(feature = "mcp")]
pub use datatab_ui_base::McpRuntimeEventRaised;
pub use datatab_ui_base::{AppStateChanged, AppStateEntity};
