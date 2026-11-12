//! UI glue for the changelog and welcome dialogs: the request the workspace
//! picks up, and the version and channel labels the dialogs and Settings print.
//!
//! The decisions themselves (which dialog opens at startup) live in
//! `datatab_app::updates`.

use datatab_app::updates;
use datatab_core::ReleaseChannel;

use crate::app_state_entity::AppStateEntity;

/// A dialog the workspace should open on behalf of another window (Settings)
/// or of the startup flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDialogRequest {
    /// "What is new": every release after `since`, or the running release
    /// alone when `since` is `None`.
    WhatsNew {
        since: Option<String>,
    },
    Welcome,
}

/// Emitted when [`AppStateEntity::pending_update_dialog`] is set. Every
/// workspace hears it, and the first one to `take()` the request opens it.
#[derive(Debug, Clone, Copy)]
pub struct UpdateDialogRequested;

impl AppStateEntity {
    pub fn request_update_dialog(
        &mut self,
        request: UpdateDialogRequest,
        cx: &mut gpui::Context<Self>,
    ) {
        self.pending_update_dialog = Some(request);
        cx.emit(UpdateDialogRequested);
        cx.notify();
    }
}

/// The version the user runs, shown in the welcome dialog and the settings status line.
pub fn running_version_label() -> String {
    updates::display_version(updates::current_version())
}

/// Lowercase channel name as printed next to the version.
pub fn channel_label(channel: ReleaseChannel) -> &'static str {
    match channel {
        ReleaseChannel::Stable => "stable",
        ReleaseChannel::Rc => "rc",
        ReleaseChannel::Nightly => "nightly",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_labels_are_lowercase_names() {
        assert_eq!(channel_label(ReleaseChannel::Stable), "stable");
        assert_eq!(channel_label(ReleaseChannel::Rc), "rc");
        assert_eq!(channel_label(ReleaseChannel::Nightly), "nightly");
    }
}
