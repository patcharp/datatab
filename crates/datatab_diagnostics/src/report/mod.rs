//! The report type: one struct that is the allowlist.
//!
//! [`ErrorReport`] holds every field a report may carry and nothing else, so
//! the serializer cannot emit a field that is not named here. The matching
//! JSON Schema lives in `schema/error-report-v1.schema.json`; the test
//! `report::tests::full_report_keys_equal_the_schema` fails when the two
//! drift apart, which is what makes a new field a deliberate change.

use datatab_core::ReleaseChannel;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod tests;

/// Version of the JSON document a report is. The server rejects any other.
pub const SCHEMA_VERSION: u32 = 1;

/// Hard cap on the serialized report. The server rejects larger bodies too.
pub const MAX_REPORT_BYTES: usize = 64 * 1024;

/// Most stack frames a report carries.
pub const MAX_FRAMES: usize = 64;

/// Most audit trail events a report carries.
pub const MAX_TRAIL_EVENTS: usize = 10;

/// What produced the report. `hang` joins this list with stage 2 (#468).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportKind {
    Panic,
    Error,
}

impl ReportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Panic => "panic",
            Self::Error => "error",
        }
    }
}

/// One stack frame: a function name when the symbol table resolves it, the
/// module's file name without any directory, and the offset inside the
/// function so a stripped build can still be mapped back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

/// One audit event, reduced to the four closed fields a report may carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrailEvent {
    pub category: String,
    pub action: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<String>,
    /// Whole seconds between this event and the report, never negative.
    pub seconds_before: i64,
}

/// One error or panic report.
///
/// Field order here is the order in the JSON; the schema test compares the key
/// sets, not the order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorReport {
    pub schema: u32,
    /// UUID v4, new for every report. Not the audit correlation id: a UUID v7
    /// carries a timestamp that would defeat `created_hour`.
    pub report_id: Uuid,
    pub kind: ReportKind,
    pub app_version: String,
    pub channel: String,
    pub os: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    pub arch: String,
    /// UTC, rounded down to the hour (`2026-11-23T05:00:00Z`).
    pub created_hour: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panic_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub frames: Vec<Frame>,
    pub trail: Vec<TrailEvent>,
}

/// Why a report could not be turned into JSON we are willing to send.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReportError {
    #[error("report serialization failed: {0}")]
    Serialization(String),
    #[error("report is {size} bytes, over the {MAX_REPORT_BYTES} byte cap")]
    TooLarge { size: usize },
}

impl ErrorReport {
    /// Starts a report with the fields every kind carries.
    ///
    /// Everything else is set by the builder in [`crate::builder`], which also
    /// runs the format checks and the scrubber.
    pub fn new(kind: ReportKind, created_hour: impl Into<String>) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            report_id: Uuid::new_v4(),
            kind,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            channel: ReleaseChannel::current().as_str().to_string(),
            os: platform_os().to_string(),
            os_version: crate::platform::os_version(),
            arch: std::env::consts::ARCH.to_string(),
            created_hour: created_hour.into(),
            error_kind: None,
            severity: None,
            driver_id: None,
            error_code: None,
            panic_message: None,
            location: None,
            frames: Vec::new(),
            trail: Vec::new(),
        }
    }

    /// Serializes the report, refusing anything over [`MAX_REPORT_BYTES`].
    pub fn to_json(&self) -> Result<Vec<u8>, ReportError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ReportError::Serialization(error.to_string()))?;

        if bytes.len() > MAX_REPORT_BYTES {
            return Err(ReportError::TooLarge { size: bytes.len() });
        }

        Ok(bytes)
    }

    /// Reads a report back, as the pending-file store and the send window do.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ReportError> {
        serde_json::from_slice(bytes).map_err(|error| ReportError::Serialization(error.to_string()))
    }
}

fn platform_os() -> &'static str {
    std::env::consts::OS
}
