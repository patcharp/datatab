//! GPUI-coupled keymap types for DataTab.
//!
//! This module contains keymap types that depend on GPUI:
//! - `actions` — GPUI action definitions
//! - `dispatcher` — Command dispatcher trait
//! - `defaults` — Default keymap bindings
//! - `chord_ext` — GPUI keystroke conversion utilities

mod actions;
mod dispatcher;

// Re-export pure keymap types from datatab_app
pub use datatab_app::keymap::{
    Command, ContextId, FocusTarget, KeyChord, KeymapLayer, KeymapStack, Modifiers,
};

#[allow(unused_imports)]
pub use datatab_app::keymap::ParseError;

// Re-export Cancel from the component crate
pub use datatab_components::actions::Cancel;

// GPUI-coupled types that stay in datatab_ui
pub use actions::*;
pub use dispatcher::CommandDispatcher;

// Keymap helpers re-exported from datatab_ui_base
pub use datatab_ui_base::keymap::key_chord_from_gpui;
pub use datatab_ui_base::keymap::{default_keymap, effective_keymap};
