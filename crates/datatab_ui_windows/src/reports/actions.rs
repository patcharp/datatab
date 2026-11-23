//! "Save as file" and "Send", the two actions a report offers besides
//! declining it.
//!
//! Both are the same for every surface: the bytes are the serialized report,
//! unchanged, and only "Send" opens a connection — one report, when the user
//! pressed the button (ADR 0008 § 2).

use std::path::PathBuf;

use datatab_diagnostics::{ErrorReport, SendError, SubmitOutcome, write_report_file};
use datatab_ui_base::{SaveTargetOutcome, SaveTargetProvider, SaveTargetRequest};

use crate::reports::offer::current_switch;

/// The file name "Save as file" suggests: the report's kind and id.
pub fn suggested_file_name(kind: &str, report_id: &str) -> String {
    format!("datatab-report-{kind}-{report_id}.json")
}

/// The same name, read from the report's own bytes. A file that does not
/// parse gets a plain name: the user is saving bytes, not a valid report.
pub fn suggested_file_name_for(bytes: &[u8]) -> String {
    match ErrorReport::from_json(bytes) {
        Ok(report) => suggested_file_name(report.kind.as_str(), &report.report_id.to_string()),
        Err(_) => "datatab-report.json".to_string(),
    }
}

/// Asks where to write a copy of `bytes` and writes it there.
///
/// `Ok(None)` means the user dismissed the picker. The bytes are written as
/// they are: the file is what the user mails, and it must match what the
/// window showed.
pub async fn save_report_copy(
    provider: Option<SaveTargetProvider>,
    suggested_name: &str,
    bytes: &[u8],
) -> Result<Option<PathBuf>, String> {
    let filter = datatab_i18n::t!("reports.save.dialog_filter");
    let dialog_title = datatab_i18n::t!("reports.save.dialog_title");
    let filter_for_dialog = filter.clone();
    let suggested_name_owned = suggested_name.to_string();

    let outcome = datatab_ui_base::file_dialog::resolve_save_target(
        provider,
        SaveTargetRequest {
            suggested_name,
            language_name: &filter,
            default_extension: "json",
        },
        async move {
            rfd::AsyncFileDialog::new()
                .set_title(dialog_title)
                .set_file_name(suggested_name_owned)
                .add_filter(filter_for_dialog, &["json"])
                .save_file()
                .await
                .map(|handle| handle.path().to_path_buf())
        },
    )
    .await;

    match outcome {
        SaveTargetOutcome::Selected { path, .. } => {
            write_report_file(&path, bytes).map_err(|error| error.to_string())?;
            Ok(Some(path))
        }
        SaveTargetOutcome::Cancelled => Ok(None),
        SaveTargetOutcome::Failed(error) => Err(error),
    }
}

/// How one send attempt is made.
///
/// Production posts through the report client. A test substitutes a function,
/// so no test opens a socket — the one thing ADR 0008 forbids a test to do.
pub type ReportSender =
    std::sync::Arc<dyn Fn(bool, &[u8]) -> Result<SubmitOutcome, SendError> + Send + Sync>;

/// The production sender.
pub fn default_sender() -> ReportSender {
    std::sync::Arc::new(send_blocking)
}

/// The one send attempt, blocking.
///
/// The caller runs it on the background executor: it opens a connection and
/// waits for the answer, which must never happen on the thread that draws.
/// The switch is read here, so a user who turned reports off between opening
/// the window and pressing Send is obeyed and no connection is opened.
pub fn send_blocking(setting_enabled: bool, body: &[u8]) -> Result<SubmitOutcome, SendError> {
    let client = datatab_diagnostics::ReportClient::new()?;
    client.submit(&current_switch(setting_enabled), body)
}
