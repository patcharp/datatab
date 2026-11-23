//! The sender client: the only code in DataTab that talks to the report service.
//!
//! Two calls exist, both to [`REPORTS_ENDPOINT`] over HTTPS and nothing else:
//! `POST /v1/reports` hands over one report, `DELETE /v1/reports/{id}` removes
//! it again with the per-report token. There is no retry, no queue and no call
//! at startup: [`ReportClient::submit`] runs only when the user pressed send,
//! and returns without opening a connection when the off switch says no.

use std::time::Duration;

use serde::Deserialize;
use uuid::Uuid;

use crate::report::MAX_REPORT_BYTES;
use crate::switch::{DisableSource, ReportSwitch, SwitchState};

#[cfg(test)]
mod tests;

/// The report service. A host of its own, so an organization can block it
/// alone.
pub const REPORTS_ENDPOINT: &str = "https://reports.datatab.pcplab.me";

/// Path one report is posted to.
const SEND_PATH: &str = "/v1/reports";

/// Whole-request deadline. A send either finishes or fails inside this.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Deadline for the TCP and TLS handshake alone.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A stored report, and the token that deletes it again.
#[derive(Clone, PartialEq, Eq)]
pub struct SubmittedReport {
    pub report_id: Uuid,
    pub delete_token: String,
}

/// The token is a credential: it never reaches a log line or a test failure
/// message, and [`SubmitOutcome`] prints through this.
impl std::fmt::Debug for SubmittedReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubmittedReport")
            .field("report_id", &self.report_id)
            .field("delete_token", &"<redacted>")
            .finish()
    }
}

/// What a submit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    Sent(SubmittedReport),
    /// Nothing was sent and no connection was opened.
    Disabled(DisableSource),
}

/// Why a send or a delete failed. Every variant is safe to show the user.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("the report endpoint must use https: {0}")]
    InsecureEndpoint(String),
    #[error("report is {size} bytes, over the {MAX_REPORT_BYTES} byte cap")]
    TooLarge { size: usize },
    #[error("report client could not start: {0}")]
    Client(String),
    #[error("report service did not answer: {0}")]
    Transport(String),
    #[error("report service answered with status {status}")]
    Status { status: u16 },
    #[error("report service sent an unusable answer: {0}")]
    Response(String),
}

/// What the service returns from `POST /v1/reports`.
#[derive(Debug, Deserialize)]
struct SendResponse {
    report_id: Uuid,
    delete_token: String,
}

/// The HTTP client for one report at a time.
#[derive(Debug)]
pub struct ReportSender {
    client: reqwest::blocking::Client,
    endpoint: String,
}

impl ReportSender {
    /// A sender for the production endpoint.
    pub fn new() -> Result<Self, SendError> {
        Self::at(REPORTS_ENDPOINT)
    }

    fn at(endpoint: &str) -> Result<Self, SendError> {
        if !endpoint.starts_with("https://") {
            return Err(SendError::InsecureEndpoint(endpoint.to_string()));
        }

        Self::with_client(endpoint, true)
    }

    /// A sender against a loopback test server. Never reachable outside tests:
    /// the production path refuses anything but HTTPS.
    #[cfg(test)]
    pub(crate) fn for_test(endpoint: &str) -> Result<Self, SendError> {
        Self::with_client(endpoint, false)
    }

    fn with_client(endpoint: &str, https_only: bool) -> Result<Self, SendError> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(format!("DataTab/{}", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(https_only)
            .build()
            .map_err(|error| SendError::Client(error.to_string()))?;

        Ok(Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
        })
    }

    /// Posts one report body and returns the id and delete token.
    pub fn send(&self, body: &[u8]) -> Result<SubmittedReport, SendError> {
        if body.len() > MAX_REPORT_BYTES {
            return Err(SendError::TooLarge { size: body.len() });
        }

        let response = self
            .client
            .post(format!("{}{SEND_PATH}", self.endpoint))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .map_err(transport)?;

        let status = response.status();
        if !status.is_success() {
            return Err(SendError::Status {
                status: status.as_u16(),
            });
        }

        let answer: SendResponse = response
            .json()
            .map_err(|error| SendError::Response(error.to_string()))?;

        Ok(SubmittedReport {
            report_id: answer.report_id,
            delete_token: answer.delete_token,
        })
    }

    /// Deletes a stored report with the token that belongs to it.
    pub fn delete(&self, report_id: Uuid, delete_token: &str) -> Result<(), SendError> {
        let response = self
            .client
            .delete(format!("{}{SEND_PATH}/{report_id}", self.endpoint))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {delete_token}"),
            )
            .send()
            .map_err(transport)?;

        let status = response.status();
        if !status.is_success() {
            return Err(SendError::Status {
                status: status.as_u16(),
            });
        }

        Ok(())
    }
}

/// The one place the off switch is honoured for sending.
///
/// The switch is read at every submit, not captured when the client is built:
/// a user who turns reports off in Settings is obeyed on the next send without
/// a restart, and turning them back on works the same way.
#[derive(Debug)]
pub struct ReportClient {
    sender: ReportSender,
}

impl ReportClient {
    pub fn new() -> Result<Self, SendError> {
        Ok(Self {
            sender: ReportSender::new()?,
        })
    }

    #[cfg(test)]
    fn with_sender(sender: ReportSender) -> Self {
        Self { sender }
    }

    /// Sends one report, or returns without a connection when reports are off.
    pub fn submit(&self, switch: &ReportSwitch, body: &[u8]) -> Result<SubmitOutcome, SendError> {
        match switch.state() {
            SwitchState::Disabled(source) => Ok(SubmitOutcome::Disabled(source)),
            SwitchState::Enabled => self.sender.send(body).map(SubmitOutcome::Sent),
        }
    }

    /// Deletes a stored report. Only a user action calls this.
    pub fn delete(&self, report_id: Uuid, delete_token: &str) -> Result<(), SendError> {
        self.sender.delete(report_id, delete_token)
    }
}

fn transport(error: reqwest::Error) -> SendError {
    if error.is_timeout() {
        SendError::Transport("timeout".to_string())
    } else {
        SendError::Transport(error.to_string())
    }
}
