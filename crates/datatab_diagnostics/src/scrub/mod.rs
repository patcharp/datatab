//! The scrubber: the only door free text passes before it enters a report.
//!
//! Three layers. The audit redactor, which knows the shapes of credentials
//! (`datatab_audit::redaction`) — its key-based JSON pass when the whole value
//! parses as JSON. Then the report rules below, which cover what the redactor
//! does not: URLs, e-mail addresses, IP addresses, host names (dotted or single
//! label, with or without a port), DSN pairs, bearer tokens and file paths
//! (absolute, home-relative, Windows, UNC and relative). A value that still
//! looks like a query or a credential after all of that is dropped whole — a
//! report never carries text we could not clear.

use regex::Regex;

#[cfg(test)]
mod tests;

/// Longest free text a report keeps, in bytes. Longer text is cut and marked.
pub const MAX_FREE_TEXT_BYTES: usize = 512;

/// What the scrubber decided about one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrubOutcome {
    /// Cleared: every rule ran and nothing suspicious is left.
    Clean(String),
    /// Could not be cleared. The caller must omit the field, never send it.
    Dropped,
}

impl ScrubOutcome {
    /// The cleared text, or `None` when the value was dropped.
    pub fn into_clean(self) -> Option<String> {
        match self {
            Self::Clean(text) => Some(text),
            Self::Dropped => None,
        }
    }
}

/// Runs every rule over `text`.
pub fn scrub(text: &str) -> ScrubOutcome {
    if text.trim().is_empty() {
        return ScrubOutcome::Clean(String::new());
    }

    let mut cleaned = redact_secrets(text);

    for (pattern, replacement) in REPLACEMENTS.iter() {
        let Ok(rule) = Regex::new(pattern) else {
            // A rule that no longer compiles must fail closed, not leak.
            return ScrubOutcome::Dropped;
        };
        cleaned = rule.replace_all(&cleaned, *replacement).into_owned();
    }

    for pattern in [LEFTOVER_QUERY, LEFTOVER_CREDENTIAL] {
        match Regex::new(pattern) {
            Ok(leftover) if leftover.is_match(&cleaned) => return ScrubOutcome::Dropped,
            Ok(_) => {}
            Err(_) => return ScrubOutcome::Dropped,
        }
    }

    ScrubOutcome::Clean(truncate(&cleaned))
}

/// The audit redactor's two passes.
///
/// When the whole value is JSON, the key-based pass runs first: it clears a
/// `password` or `token` key by its name, not by what its value happens to look
/// like. Any other text gets the pattern pass, which knows credentials inside a
/// URI, AWS keys, JWTs and long hex strings.
fn redact_secrets(text: &str) -> String {
    if serde_json::from_str::<serde_json::Value>(text).is_ok() {
        datatab_audit::redaction::redact_json(text, true).redacted
    } else {
        datatab_audit::redaction::redact_error_message(text, true).redacted
    }
}

/// Cuts an over-long value at a character boundary and marks the cut, so the
/// cap itself can never split a UTF-8 sequence.
pub fn truncate(text: &str) -> String {
    if text.len() <= MAX_FREE_TEXT_BYTES {
        return text.to_string();
    }

    let mut end = MAX_FREE_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}<truncated>", &text[..end])
}

/// One replacement rule: the shape, and what replaces it.
///
/// Order matters. URLs go first because they carry a host, a port and often a
/// path. Paths go before host names so a path segment is not mistaken for a
/// host. The trailing rules cover the shapes a driver or a user writes into a
/// message. Patterns are compiled at use rather than once: a report is rare, and
/// a pattern that fails to compile then drops the value instead of panicking.
const REPLACEMENTS: &[(&str, &str)] = &[
    // A URL carries host, port and often a path, so it goes first and whole;
    // the redactor has already cleared any credentials inside it.
    (r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s"'<>)]+"#, "<url>"),
    // E-mail addresses.
    (r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "<email>"),
    // A Windows path. It crosses spaces, so `C:\Program Files\...` loses its
    // tail, and stops at a quote, a tag or a line break.
    (r#"(?i)\b[a-z]:\\[^"'<>\r\n]*"#, "<path>"),
    // A UNC path.
    (r#"\\\\[^"'<>\r\n]+"#, "<path>"),
    // A home-relative path.
    (r#"~[/\\][^"'<>\r\n]*"#, "<path>"),
    // An absolute Unix path: a leading slash, then at least two segments of any
    // non-separator characters, so non-ASCII names and dots are covered.
    (r#"(^|[\s"'=(])(/(?:[^/\s"'<>]+/)+[^/\s"'<>]*)"#, "$1<path>"),
    // A relative path with at least one separator (`data/x`, `src/de.rs`).
    (r"\b[\w.-]+(?:/[\w.-]+)+", "<path>"),
    // A source file named in a message (`main.rs:184`, `Cargo.toml`). It is a
    // path, not a host, so it goes before the host rules.
    (r"\w+\.(rs|toml|json|lock|md|sql)(:\d+)?", "<path>"),
    // Bracketed IPv6 (`[::1]`) before the bare form.
    (r"\[[0-9A-Fa-f:]{2,39}\]", "<ip>"),
    // Bare IPv6: a hex group followed by at least two colon-led groups, so the
    // `::` shorthand is covered and a bare number is not.
    (r"(?i)\b[0-9a-f]{1,4}(?::[0-9a-f]{0,4}){2,7}\b", "<ip>"),
    // IPv4, with or without an appended port.
    (r"\b\d{1,3}(?:\.\d{1,3}){3}(?::\d{1,5})?\b", "<ip>"),
    // A DSN pair: `host=...`, `user=...`, `dbname: ...`. A short underscore
    // prefix (`DB_HOST=`) is part of the key, and `support=1` is not.
    (
        r#"(?i)(^|[^\w])(?:\w+_)?(host|hostname|server|port|user|username|database|dbname)\s*[:=]\s*(?:'[^']*'|"[^"]*"|[^\s,;]*)"#,
        "$1$2=<value>",
    ),
    // What a driver writes in words: `for user alice`, `user "alice"`,
    // `host name "dbhost"`.
    (r"(?i)\bfor user\s+\S+", "for user <value>"),
    (r#"(?i)\b(?:user|username)\s+"[^"]*""#, "user <value>"),
    (
        r#"(?i)\b(?:host|hostname)(?:\s+name)?\s+"[^"]*""#,
        "host <value>",
    ),
    // A single-label host with a port (`postgres:5432`). Dotted hosts are next.
    (r"\b[A-Za-z][\w-]*:\d{1,5}\b", "<host>"),
    // A dotted host name (`db.internal`, `h.example.com`).
    (
        r"\b(?:[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z]{2,}\b",
        "<host>",
    ),
    // A bearer token, however short.
    (r"(?i)\bbearer\s+\S+", "Bearer <redacted>"),
];

/// Query text that survived the replacement rules. The message is dropped
/// rather than sent with the statement in it.
const LEFTOVER_QUERY: &str = r#"(?i)\b(?:select\s+[^;]{0,400}\sfrom|insert\s+into|update\s+[\w."]+\s+set|delete\s+from|create\s+table|alter\s+table|drop\s+table)\b"#;

/// A credential we could not clear: a secret-looking key — also as part of a
/// longer name such as `PGPASSWORD` or `DB_PASSWORD` — followed by a value.
const LEFTOVER_CREDENTIAL: &str =
    r#"(?i)\w*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key)\w*["']?\s*[:=]\s*\S"#;
