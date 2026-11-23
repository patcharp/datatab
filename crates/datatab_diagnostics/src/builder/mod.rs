//! The builder: turns what the app knows into a report that passes the
//! allowlist, the format checks and the scrubber.
//!
//! Every value that can vary is either a closed set (`error_kind`,
//! `severity`, `driver_id`, `action`, `error_code`) or goes through
//! [`crate::scrub`]. A value that fails its check is dropped, never replaced
//! with something invented.

use chrono::{DateTime, Utc};

use crate::report::{ErrorReport, Frame, MAX_FRAMES, MAX_TRAIL_EVENTS, ReportKind, TrailEvent};
use crate::scrub;

#[cfg(test)]
mod tests;

/// The `ErrorKind` names a report may carry.
///
/// `ErrorKind` lives in `datatab_ui_base`, which this crate must not depend on,
/// so the wire names are listed here; a name outside the list is dropped. The
/// test `datatab_ui_base::user_error::tests::every_error_kind_name_is_reportable`
/// binds this list to `ErrorKind::as_str`, so a new variant cannot be dropped
/// silently.
pub const ERROR_KINDS: &[&str] = &[
    "storage", "network", "auth", "hook", "driver", "user", "config",
];

/// A panic payload, classified by what it actually is.
///
/// Only fixed text from our own code may be sent. A formatted `String` can hold
/// row data, a path or a statement, so the report keeps its length and nothing
/// else; this is the rule from ADR 0008 § 1.
#[derive(Debug, Clone, Copy)]
pub enum PanicMessage<'a> {
    /// `&'static str`: a literal from the source.
    Static(&'a str),
    /// `String`: built at run time, so its content is unknown.
    Formatted(&'a str),
    /// Neither: nothing we can describe.
    Unknown,
}

/// Where the panic happened, before reduction.
#[derive(Debug, Clone)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
}

/// Captures the current stack for a report.
///
/// Names resolve only when the build keeps its symbol table, which the release
/// profile does for exactly this reason (ADR 0008 § Symbol plan).
pub fn capture_backtrace() -> backtrace::Backtrace {
    backtrace::Backtrace::new()
}

/// Everything the panic hook has to give.
pub struct PanicInput<'a> {
    pub message: PanicMessage<'a>,
    pub location: Option<SourceLocation>,
    pub backtrace: backtrace::Backtrace,
    pub trail: Vec<datatab_audit::RecentEvent>,
    pub now: DateTime<Utc>,
}

/// Everything a user-facing error has to give.
pub struct ErrorInput {
    /// `ErrorKind::as_str()` from the caller.
    pub error_kind: String,
    pub severity: datatab_core::observability::EventSeverity,
    pub driver_id: Option<String>,
    pub error_code: Option<String>,
    pub trail: Vec<datatab_audit::RecentEvent>,
    pub now: DateTime<Utc>,
}

/// Builds the report a panic produces.
pub fn build_panic_report(input: PanicInput<'_>) -> ErrorReport {
    let now_ms = input.now.timestamp_millis();
    let mut report = ErrorReport::new(ReportKind::Panic, created_hour(input.now));

    report.panic_message = panic_message(input.message);
    report.location = input
        .location
        .as_ref()
        .and_then(|location| reduce_location(&location.file, location.line));
    report.frames = frames(&input.backtrace);
    report.trail = trail(&input.trail, now_ms);

    report
}

/// Builds the report a user-facing error produces.
pub fn build_error_report(input: ErrorInput) -> ErrorReport {
    let now_ms = input.now.timestamp_millis();
    let mut report = ErrorReport::new(ReportKind::Error, created_hour(input.now));

    report.error_kind = ERROR_KINDS
        .iter()
        .find(|name| **name == input.error_kind)
        .map(|name| (*name).to_string());
    report.severity = Some(input.severity.as_str().to_string());
    report.driver_id = input.driver_id.as_deref().and_then(closed_driver_id);
    report.error_code = input.error_code.as_deref().and_then(closed_error_code);
    report.trail = trail(&input.trail, now_ms);

    report
}

/// `2026-11-23T05:00:00Z`: the UTC hour the report was made in, never finer.
pub fn created_hour(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%dT%H:00:00Z").to_string()
}

fn panic_message(message: PanicMessage<'_>) -> Option<String> {
    match message {
        PanicMessage::Static(text) => scrub::scrub(text).into_clean().filter(|t| !t.is_empty()),
        PanicMessage::Formatted(text) => Some(format!("<dropped: {} chars>", text.chars().count())),
        PanicMessage::Unknown => None,
    }
}

/// Reduces a source path to the two shapes ADR 0008 allows.
///
/// A workspace path keeps everything from `crates/` on. A dependency path keeps
/// everything from the crate directory (`serde-1.0.0/src/de.rs`). Anything else
/// is `<external>`, so a home directory, a username or a drive letter never
/// reaches a report.
pub fn reduce_location(file: &str, line: u32) -> Option<String> {
    let normalized = file.replace('\\', "/");
    let reduced = if let Some(rest) = normalized.split_once("crates/") {
        format!("crates/{}", rest.1)
    } else if let Some(rest) = registry_crate_path(&normalized) {
        rest
    } else if let Some(rest) = normalized.split_once("vendor/") {
        format!("vendor/{}", rest.1)
    } else {
        return Some(format!("<external>:{line}"));
    };

    Some(format!("{reduced}:{line}"))
}

/// `.../registry/src/<hash>/serde-1.0.0/src/de.rs` becomes
/// `serde-1.0.0/src/de.rs`.
fn registry_crate_path(path: &str) -> Option<String> {
    let (_, after_registry) = path.split_once("registry/src/")?;
    let (_, after_hash) = after_registry.split_once('/')?;

    if after_hash.is_empty() {
        None
    } else {
        Some(after_hash.to_string())
    }
}

/// Closes `driver_id` to a built-in driver id, or `rpc` for anything external.
///
/// `rpc:<socket_id>` never survives: the socket id names a local file, and a
/// report names no path.
pub fn closed_driver_id(driver_id: &str) -> Option<String> {
    if datatab_core::DbKind::is_builtin_driver_id(driver_id) {
        return Some(driver_id.to_string());
    }

    if driver_id == "rpc" || driver_id.starts_with("rpc:") {
        return Some("rpc".to_string());
    }

    None
}

/// `^[A-Za-z0-9_.-]{1,16}$`, as ADR 0008 sets it.
pub fn closed_error_code(code: &str) -> Option<String> {
    let valid = !code.is_empty()
        && code.len() <= 16
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));

    valid.then(|| code.to_string())
}

/// Up to [`MAX_FRAMES`] frames, names kept, directories removed.
pub fn frames(backtrace: &backtrace::Backtrace) -> Vec<Frame> {
    let mut frames = Vec::new();

    'outer: for frame in backtrace.frames() {
        for symbol in frame.symbols() {
            if frames.len() == MAX_FRAMES {
                break 'outer;
            }

            frames.push(Frame {
                function: symbol.name().and_then(|name| symbol_name(&name)),
                module: symbol
                    .filename()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned()),
                offset: offset_within(symbol, frame),
            });
        }
    }

    frames
}

fn trail(events: &[datatab_audit::RecentEvent], now_ms: i64) -> Vec<TrailEvent> {
    let mut ordered: Vec<&datatab_audit::RecentEvent> = events.iter().collect();
    ordered.sort_by_key(|event| event.ts_ms);

    let kept = ordered.len().saturating_sub(MAX_TRAIL_EVENTS);
    ordered
        .into_iter()
        .skip(kept)
        .map(|event| TrailEvent {
            category: event.category.as_str().to_string(),
            action: closed_action(&event.action),
            outcome: event.outcome.as_str().to_string(),
            driver_id: event.driver_id.as_deref().and_then(closed_driver_id),
            seconds_before: ((now_ms - event.ts_ms) / 1000).max(0),
        })
        .collect()
}

/// A built-in audit action constant, or `other`.
fn closed_action(action: &str) -> String {
    if datatab_core::observability::actions::is_known_action(action) {
        action.to_string()
    } else {
        "other".to_string()
    }
}

/// Demangled and reduced to characters a symbol name can hold, so a name can
/// never smuggle in a path or a statement.
fn symbol_name(name: &backtrace::SymbolName<'_>) -> Option<String> {
    let demangled = format!("{name:#}");
    let cleaned: String = demangled
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '_' | ':'
                        | '<'
                        | '>'
                        | ','
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '('
                        | ')'
                        | '&'
                        | '*'
                        | '#'
                        | '~'
                        | '\''
                        | '='
                        | '-'
                        | ' '
                        | '.'
                )
        })
        .collect();

    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// How far into its own function a frame is: the address minus the symbol
/// start. Independent of where the image was loaded, so it stays meaningful
/// across runs.
fn offset_within(
    symbol: &backtrace::BacktraceSymbol,
    frame: &backtrace::BacktraceFrame,
) -> Option<u64> {
    let start = symbol.addr()? as u64;
    let address = frame.ip() as u64;

    (start != 0).then_some(address.saturating_sub(start))
}
