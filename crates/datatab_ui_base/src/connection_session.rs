//! Fence for background lookups tied to a profile's live connection.
//!
//! [`datatab_core::Connection::instance_catalog`] probes privileges on the
//! client connection, so callers run it on the background executor. The caller
//! captures the connection and the profile's session generation first, and
//! applies the result on the foreground only while the profile still holds that
//! session. A reconnect in between drops the result.

use crate::AppStateEntity;
use datatab_core::Connection;
use std::sync::Arc;
use uuid::Uuid;

/// The connection a background lookup was started for, plus the profile's
/// session generation at that moment.
pub struct ConnectionSession {
    /// The connection the lookup runs on.
    pub connection: Arc<dyn Connection>,
    /// The profile's session generation, when it has one.
    pub generation: Option<u64>,
}

impl AppStateEntity {
    /// Captures the profile's live connection and session generation, or
    /// `None` while the profile is disconnected.
    pub fn capture_connection_session(&self, profile_id: Uuid) -> Option<ConnectionSession> {
        let connection = self
            .connections()
            .get(&profile_id)
            .map(|connected| connected.connection.clone())?;
        Some(ConnectionSession {
            connection,
            generation: self.profile_session_generation(profile_id),
        })
    }

    /// Returns `true` while the profile still holds the captured connection and
    /// session generation.
    pub fn is_same_connection_session(
        &self,
        profile_id: Uuid,
        expected: &ConnectionSession,
    ) -> bool {
        let Some(current) = self.connections().get(&profile_id) else {
            return false;
        };
        if !Arc::ptr_eq(&current.connection, &expected.connection) {
            return false;
        }
        match expected.generation {
            Some(generation) => self.profile_session_generation(profile_id) == Some(generation),
            None => true,
        }
    }
}
