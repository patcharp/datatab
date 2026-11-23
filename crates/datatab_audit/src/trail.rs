//! The audit fields a diagnostic report may carry.
//!
//! ADR 0008 reduces the trail to four closed fields per event: `category`,
//! `action`, `outcome` and `driver_id`, plus how long before the report the
//! event happened. The audit `summary`, `object_id`, `details_json`, error
//! message and correlation id never leave the machine.

use datatab_core::observability::{EventCategory, EventOutcome};

/// One audit event, reduced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEvent {
    pub ts_ms: i64,
    pub category: EventCategory,
    pub action: String,
    pub outcome: EventOutcome,
    pub driver_id: Option<String>,
}

impl RecentEvent {
    /// Builds an event from stored strings.
    ///
    /// `None` when a closed field does not parse or no action is stored: the
    /// row is skipped rather than sent with a guessed value.
    pub fn from_stored(
        ts_ms: i64,
        category: Option<&str>,
        action: Option<&str>,
        outcome: Option<&str>,
        driver_id: Option<&str>,
    ) -> Option<Self> {
        let action = action.filter(|value| !value.is_empty())?;

        Some(Self {
            ts_ms,
            category: EventCategory::from_str_repr(category?)?,
            action: action.to_string(),
            outcome: EventOutcome::from_str_repr(outcome?)?,
            driver_id: driver_id.map(str::to_string),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_row_round_trips_its_closed_fields() {
        let event = RecentEvent::from_stored(
            1_700_000_000_000,
            Some("query"),
            Some("query_execute_failed"),
            Some("failure"),
            Some("postgres"),
        )
        .expect("row parses");

        assert_eq!(event.category, EventCategory::Query);
        assert_eq!(event.outcome, EventOutcome::Failure);
        assert_eq!(event.action, "query_execute_failed");
        assert_eq!(event.driver_id.as_deref(), Some("postgres"));
    }

    #[test]
    fn unknown_closed_field_skips_the_row() {
        assert!(
            RecentEvent::from_stored(
                0,
                Some("nonsense"),
                Some("query_execute"),
                Some("success"),
                None
            )
            .is_none()
        );
        assert!(
            RecentEvent::from_stored(
                0,
                Some("query"),
                Some("query_execute"),
                Some("nonsense"),
                None
            )
            .is_none()
        );
        assert!(
            RecentEvent::from_stored(0, Some("query"), Some(""), Some("success"), None).is_none()
        );
        assert!(
            RecentEvent::from_stored(0, None, Some("query_execute"), Some("success"), None)
                .is_none()
        );
    }
}
