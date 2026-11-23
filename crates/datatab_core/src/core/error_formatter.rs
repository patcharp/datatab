use crate::DbError;
use std::fmt;

/// Formatted error with structured information for display.
#[derive(Debug, Clone, Default)]
pub struct FormattedError {
    /// Primary error message.
    pub message: String,

    /// Additional detail about the error (e.g., PostgreSQL's DETAIL field).
    pub detail: Option<String>,

    /// Suggestion for how to fix the error (e.g., PostgreSQL's HINT field).
    pub hint: Option<String>,

    /// Error code from the database (e.g., SQLSTATE, MySQL error code).
    pub code: Option<String>,

    /// Location information if available.
    pub location: Option<ErrorLocation>,

    /// Where in the submitted statement the error points, when the driver can
    /// say. Left at `None` by a driver that reports no position, and the
    /// editor then leaves the cursor where it is.
    pub position: Option<ErrorPosition>,

    /// Whether the error is retriable (e.g., transient network issues).
    pub retriable: bool,
}

impl FormattedError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Default::default()
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_location(mut self, location: ErrorLocation) -> Self {
        self.location = Some(location);
        self
    }

    pub fn with_position(mut self, position: ErrorPosition) -> Self {
        self.position = Some(position);
        self
    }

    pub fn with_retriable(mut self, retriable: bool) -> Self {
        self.retriable = retriable;
        self
    }

    /// Convert to a single-line display string.
    pub fn to_display_string(&self) -> String {
        let mut parts = vec![self.message.clone()];

        if let Some(ref detail) = self.detail {
            parts.push(format!("Detail: {}", detail));
        }

        if let Some(ref hint) = self.hint {
            parts.push(format!("Hint: {}", hint));
        }

        if let Some(ref loc) = self.location {
            if let Some(ref table) = loc.table {
                parts.push(format!("Table: {}", table));
            }
            if let Some(ref column) = loc.column {
                parts.push(format!("Column: {}", column));
            }
            if let Some(ref constraint) = loc.constraint {
                parts.push(format!("Constraint: {}", constraint));
            }
        }

        if let Some(ref code) = self.code {
            parts.push(format!("Code: {}", code));
        }

        parts.join(". ")
    }

    /// Classify into the appropriate `DbError` variant for query errors.
    ///
    /// Uses SQLSTATE codes (when present) to route to semantic variants:
    /// - `42xxx` -> `SyntaxError`
    /// - `23xxx` -> `ConstraintViolation`
    /// - `28xxx` -> `AuthFailed`
    /// - `42501` / `42000` (with permission context) -> `PermissionDenied`
    /// - `42P01` / `1146` -> `ObjectNotFound`
    /// - Everything else -> `QueryFailed`
    pub fn into_query_error(self) -> DbError {
        if let Some(variant) = self.code.as_deref().and_then(classify_query_sqlstate) {
            return match variant {
                ErrorClass::Syntax => DbError::SyntaxError(self),
                ErrorClass::Constraint => DbError::ConstraintViolation(self),
                ErrorClass::Auth => DbError::AuthFailed(self),
                ErrorClass::Permission => DbError::PermissionDenied(self),
                ErrorClass::NotFound => DbError::ObjectNotFound(self),
            };
        }

        DbError::QueryFailed(self)
    }

    /// Classify into the appropriate `DbError` variant for connection errors.
    ///
    /// Uses SQLSTATE codes (when present) to route to semantic variants:
    /// - `28xxx` -> `AuthFailed`
    /// - Everything else -> `ConnectionFailed`
    pub fn into_connection_error(self) -> DbError {
        if self.code.as_deref().is_some_and(|c| c.starts_with("28")) {
            return DbError::AuthFailed(self);
        }

        DbError::ConnectionFailed(self)
    }
}

impl fmt::Display for FormattedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_display_string())
    }
}

impl From<String> for FormattedError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for FormattedError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

/// Where in the submitted statement an error points, as far as the driver can
/// say.
///
/// The two fields are independent and optional: a driver fills the one its
/// engine reports and leaves the other at `None`. All `None` means "this
/// driver did not say", which is not the same as "the error is at the start".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErrorPosition {
    /// Offset of the failing character from the start of the submitted
    /// statement, counted in Unicode scalar values rather than bytes, so it
    /// means the same on every engine.
    pub character_offset: Option<usize>,

    /// Line of the failing text in the submitted statement, 1-based, for a
    /// server that reports a line without an offset.
    pub line: Option<u32>,
}

impl ErrorPosition {
    /// A position from a server that counts the first character of the
    /// statement as 1, the way PostgreSQL's and Redshift's `position` field
    /// does. This is the one place that removes the leading one.
    ///
    /// A server that sends 0 has no position to report, so the result carries
    /// no offset.
    pub fn from_one_based_character(one_based: u32) -> Self {
        Self {
            character_offset: one_based.checked_sub(1).map(|offset| offset as usize),
            line: None,
        }
    }

    /// A position from a 0-based byte offset into `statement`, the way SQLite
    /// counts `sqlite3_error_offset`. The byte offset becomes a character
    /// offset here, against the same statement text, and is clamped to it, so
    /// a number that points outside the statement cannot panic a caller.
    pub fn from_byte_offset(statement: &str, byte_offset: usize) -> Self {
        let mut byte = byte_offset.min(statement.len());
        while !statement.is_char_boundary(byte) {
            byte -= 1;
        }
        Self {
            character_offset: Some(statement[..byte].chars().count()),
            line: None,
        }
    }

    /// A position from a 1-based line alone, the way MySQL states a syntax
    /// error. A line of 0 has nothing to report.
    pub fn from_line(line: u32) -> Self {
        Self {
            character_offset: None,
            line: (line > 0).then_some(line),
        }
    }

    /// Resolves this position against the statement the driver ran into a
    /// 0-based character offset into it.
    ///
    /// An offset wins over a line, since it is the more precise of the two.
    /// Either is clamped to the statement, so a position past its end lands on
    /// the end rather than nowhere. `None` when neither field is known.
    pub fn resolve_character_offset(&self, statement: &str) -> Option<usize> {
        if let Some(offset) = self.character_offset {
            return Some(offset.min(statement.chars().count()));
        }

        let line = self.line?;
        let mut line_start = 0;
        for _ in 1..line {
            match statement[line_start..].find('\n') {
                Some(newline) => line_start += newline + 1,
                None => return Some(statement.chars().count()),
            }
        }
        Some(statement[..line_start].chars().count())
    }
}

enum ErrorClass {
    Syntax,
    Constraint,
    Auth,
    Permission,
    NotFound,
}

/// Classify a SQLSTATE or MySQL error code into a semantic error class.
fn classify_query_sqlstate(code: &str) -> Option<ErrorClass> {
    // Exact matches first (more specific)
    match code {
        // PostgreSQL: insufficient_privilege
        "42501" => return Some(ErrorClass::Permission),
        // PostgreSQL: undefined_table
        "42P01" => return Some(ErrorClass::NotFound),
        // PostgreSQL: undefined_column
        "42703" => return Some(ErrorClass::NotFound),
        // PostgreSQL: undefined_function
        "42883" => return Some(ErrorClass::NotFound),
        // MySQL: Table doesn't exist
        "1146" => return Some(ErrorClass::NotFound),
        // MySQL: Unknown column
        "1054" => return Some(ErrorClass::NotFound),
        // MySQL: Access denied
        "1044" | "1045" => return Some(ErrorClass::Auth),
        _ => {}
    }

    // Class-level matching (first 2 characters of SQLSTATE)
    if code.len() >= 2 {
        match &code[..2] {
            // Class 23: Integrity constraint violation
            "23" => return Some(ErrorClass::Constraint),
            // Class 28: Invalid authorization specification
            "28" => return Some(ErrorClass::Auth),
            // Class 42: Syntax error or access rule violation
            "42" => return Some(ErrorClass::Syntax),
            _ => {}
        }
    }

    None
}

/// Location information for database errors.
#[derive(Debug, Clone, Default)]
pub struct ErrorLocation {
    /// Schema where the error occurred.
    pub schema: Option<String>,

    /// Table where the error occurred.
    pub table: Option<String>,

    /// Column where the error occurred.
    pub column: Option<String>,

    /// Constraint that was violated.
    pub constraint: Option<String>,
}

impl ErrorLocation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    pub fn with_column(mut self, column: impl Into<String>) -> Self {
        self.column = Some(column.into());
        self
    }

    pub fn with_constraint(mut self, constraint: impl Into<String>) -> Self {
        self.constraint = Some(constraint.into());
        self
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.schema.is_none()
            && self.table.is_none()
            && self.column.is_none()
            && self.constraint.is_none()
    }
}

/// Trait for formatting database-specific errors into a structured format.
///
/// Each driver implements this to extract detailed error information
/// from their specific error types.
pub trait QueryErrorFormatter: Send + Sync {
    /// Format a query execution error.
    ///
    /// This is called when a SQL query or database operation fails.
    fn format_query_error(&self, error: &(dyn std::error::Error + 'static)) -> FormattedError;
}

/// Trait for formatting connection errors.
///
/// Separated from QueryErrorFormatter because connection errors often need
/// additional context (host, port) that query errors don't have.
pub trait ConnectionErrorFormatter: Send + Sync {
    /// Format a connection error with host/port context.
    fn format_connection_error(
        &self,
        error: &(dyn std::error::Error + 'static),
        host: &str,
        port: u16,
    ) -> FormattedError;

    /// Format a URI-based connection error.
    ///
    /// The URI should be sanitized (password removed) before display.
    fn format_uri_error(
        &self,
        error: &(dyn std::error::Error + 'static),
        sanitized_uri: &str,
    ) -> FormattedError;
}

/// Default implementation that just uses Display.
pub struct DefaultErrorFormatter;

impl QueryErrorFormatter for DefaultErrorFormatter {
    fn format_query_error(&self, error: &(dyn std::error::Error + 'static)) -> FormattedError {
        FormattedError::new(error.to_string())
    }
}

impl ConnectionErrorFormatter for DefaultErrorFormatter {
    fn format_connection_error(
        &self,
        error: &(dyn std::error::Error + 'static),
        host: &str,
        port: u16,
    ) -> FormattedError {
        FormattedError::new(format!("Failed to connect to {}:{}: {}", host, port, error))
    }

    fn format_uri_error(
        &self,
        error: &(dyn std::error::Error + 'static),
        sanitized_uri: &str,
    ) -> FormattedError {
        FormattedError::new(format!(
            "Failed to connect using URI {}: {}",
            sanitized_uri, error
        ))
    }
}

/// Sanitize a connection URI by removing credentials.
///
/// Returns a safe-to-display version of the URI with the userinfo password and
/// credential-bearing parameters replaced by `***`. See
/// [`redact_uri_credentials`] for the exact rules.
pub fn sanitize_uri(uri: &str) -> String {
    redact_uri_credentials(uri, "***").0
}

/// Parameter names whose values are credentials when they appear in the query
/// string or in `;`-separated parameters of a URL. Compared case-insensitively.
const SENSITIVE_URI_PARAMETERS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "pass",
    "token",
    "access_token",
    "auth_token",
    "authtoken",
    "refresh_token",
    "id_token",
    "secret",
    "client_secret",
    "secret_key",
    "api_key",
    "apikey",
    "sessiontoken",
    "session_token",
    "x-amz-security-token",
    "x-amz-signature",
    "sslpassword",
    "oauth_client_secret",
    "scram_client_key",
    "scram_server_key",
];

/// libpq keys whose value is itself a secret. Every key is also in
/// `SENSITIVE_URI_PARAMETERS`; a test guards that. The masking pass does not
/// read this list: it masks every key that is not in `LIBPQ_PUBLIC_KEYS`.
pub const LIBPQ_SECRET_KEYS: [&str; 5] = [
    "password",
    "sslpassword",
    "oauth_client_secret",
    "scram_client_key",
    "scram_server_key",
];

/// libpq connection parameters whose value is safe to show in an error or a
/// log line (host names, ports, modes, file paths). A secret key never belongs
/// here; a test checks that the two lists do not overlap.
pub const LIBPQ_PUBLIC_KEYS: [&str; 44] = [
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "passfile",
    "connect_timeout",
    "client_encoding",
    "application_name",
    "fallback_application_name",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_count",
    "keepalives_retries",
    "tcp_user_timeout",
    "sslmode",
    "sslnegotiation",
    "sslcompression",
    "sslcert",
    "sslkey",
    "sslkeylogfile",
    "sslrootcert",
    "sslcrl",
    "sslcrldir",
    "sslsni",
    "requirepeer",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "krbsrvname",
    "gsslib",
    "gssencmode",
    "gssdelegation",
    "channel_binding",
    "require_auth",
    "service",
    "target_session_attrs",
    "load_balance_hosts",
    "replication",
    "oauth_issuer",
    "oauth_client_id",
    "oauth_scope",
    "min_protocol_version",
    "max_protocol_version",
];

/// Masks the password of the userinfo of every `scheme://` in `text`, before
/// any other pass can change where the userinfo ends.
///
/// The userinfo runs from after `://` to the LAST `@` before the next
/// whitespace (or the end of the text), and everything after its first `:`
/// becomes `replacement`. `;`, `&`, `=` and `?` are legal inside a password and
/// do not end it. When a `/`, `?` or `#` comes before that `@`, the `@` may
/// belong to the path or the query, so everything after the first `:` up to the
/// whitespace is masked, the host included. It masks too much; it never shows
/// part of a password. A userinfo without `:` is left as it is.
pub fn redact_userinfo_passwords(text: &str, replacement: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(offset) = text[cursor..].find("://") {
        let start = cursor + offset + 3;
        let end = text[start..]
            .find(char::is_whitespace)
            .map_or(text.len(), |length| start + length);
        let userinfo_end = text[start..end].rfind('@').map(|at| start + at);
        let colon = userinfo_end.and_then(|at| text[start..at].find(':').map(|c| start + c));

        let (Some(at), Some(colon)) = (userinfo_end, colon) else {
            output.push_str(&text[cursor..start]);
            cursor = start;
            continue;
        };
        output.push_str(&text[cursor..=colon]);
        output.push_str(replacement);
        // An `@` after a `/`, `?` or `#` may sit in the path or the query, so
        // the real userinfo end is unknown: mask to the end of the token.
        let may_be_path = text[start..at].contains(['/', '?', '#']);
        cursor = if may_be_path { end } else { at };
    }

    output.push_str(&text[cursor..]);
    output
}

/// Whether `key` is a libpq parameter whose value may be shown, compared
/// case-insensitively. Every other key, a misspelled one included, is masked.
pub fn is_libpq_public_key(key: &str) -> bool {
    LIBPQ_PUBLIC_KEYS
        .iter()
        .any(|public_key| key.eq_ignore_ascii_case(public_key))
}

/// Byte length of the libpq keyword value at the start of `input`.
///
/// A value that starts with `'` runs to its closing unescaped `'`. An unquoted
/// value ends only at whitespace or the end of the text. A backslash escapes
/// the next character in both forms. `;`, `&` and `'` are legal inside a
/// password, so they never end an unquoted value.
pub fn libpq_value_end(input: &str) -> usize {
    libpq_value_scan(input).0
}

/// Like `libpq_value_end`, and whether a quoted value ended at its own
/// unescaped closing `'`. An unquoted or unclosed value reports `false`.
fn libpq_value_scan(input: &str) -> (usize, bool) {
    let quoted = input.starts_with('\'');
    let mut escaped = false;
    for (offset, character) in input.char_indices().skip(usize::from(quoted)) {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if quoted && character == '\'' {
            return (offset + character.len_utf8(), true);
        } else if !quoted && character.is_whitespace() {
            return (offset, false);
        }
    }
    (input.len(), false)
}

/// Masks every value in `text` that is not provably public. It is fail-closed.
///
/// A key is the whole run of characters before an `=` (spaces around the `=`
/// are allowed), back to the nearest delimiter: whitespace, `?`, `&`, `;`, `'`,
/// `"` or the start of the text. Its characters do not matter. It is compared
/// case-insensitively with `LIBPQ_PUBLIC_KEYS`.
///
/// A value is shown only when its key is public and the value holds no `=`. A
/// quoted value runs to its closing unescaped `'`; any other value runs to
/// whitespace or the end of the text (`;`, `&` and `'` are legal inside a
/// password). Every other value is replaced up to that end, and the quotes of a
/// quoted value stay. So `?sslmode=require&password=x` becomes `?sslmode=***`:
/// the pass masks too much rather than too little.
///
/// Run it on the original text, before `sanitize_uri`: nothing is skipped
/// because it looks masked.
pub fn redact_libpq_secret_values(text: &str, replacement: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut index = 0;

    while index < text.len() {
        let Some(character) = text[index..].chars().next() else {
            break;
        };
        if is_key_delimiter(character) {
            index += character.len_utf8();
            continue;
        }
        let key_end = text[index..]
            .find(|c: char| is_key_delimiter(c) || c == '=')
            .map_or(text.len(), |length| index + length);
        let Some(after_equals) = text[key_end..].trim_start().strip_prefix('=') else {
            index = key_end.max(index + character.len_utf8());
            continue;
        };
        let value_start = text.len() - after_equals.trim_start().len();
        let (inner_start, inner_end, value_end) = value_bounds(text, value_start);
        let shown = is_libpq_public_key(&text[index..key_end])
            && !text[inner_start..inner_end].contains('=');
        if !shown && inner_end > inner_start {
            output.push_str(&text[cursor..inner_start]);
            output.push_str(replacement);
            cursor = inner_end;
        }
        index = value_end.max(value_start);
    }

    output.push_str(&text[cursor..]);
    output
}

fn is_key_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, '?' | '&' | ';' | '\'' | '"')
}

/// For the value that starts at `value_start`: the byte range of its content
/// (inside the quotes of a quoted value) and the byte offset where it ends.
fn value_bounds(text: &str, value_start: usize) -> (usize, usize, usize) {
    let rest = &text[value_start..];
    let (length, closed) = libpq_value_scan(rest);
    let value_end = value_start + length;
    if !rest.starts_with('\'') {
        return (value_start, value_end, value_end);
    }
    (
        value_start + 1,
        if closed { value_end - 1 } else { value_end },
        value_end,
    )
}

/// Redacts credentials from every URL embedded in `text`, whatever its scheme.
///
/// For each `scheme://` occurrence it replaces:
/// - the password of the userinfo (`scheme://user:<password>@host:port`), keeping the
///   user name and host so the message stays useful. The last `@` of the
///   authority ends the userinfo, so an unencoded `@` inside the password is
///   still covered, and bracketed IPv6 hosts are left intact;
/// - the value of credential-bearing parameters (`password=`, `pwd=`,
///   `token=`, `secret=`, ...) in the query string or in JDBC-style
///   `;key=value` segments.
///
/// A URL ends at whitespace, a quote or an angle bracket, so the function can
/// run over free text such as error messages and log lines. A userinfo without
/// a password (`scheme://user@host`) is left unchanged.
///
/// Returns the redacted text and the number of values replaced.
pub fn redact_uri_credentials(text: &str, replacement: &str) -> (String, usize) {
    let mut output = String::with_capacity(text.len());
    let mut redaction_count = 0;
    let mut cursor = 0;

    while let Some(offset) = text[cursor..].find("://") {
        let separator_start = cursor + offset;

        if !has_scheme_before(&text[..separator_start]) {
            output.push_str(&text[cursor..separator_start + 3]);
            cursor = separator_start + 3;
            continue;
        }

        let url_start = separator_start + 3;
        let url_end = text[url_start..]
            .find(is_url_terminator)
            .map_or(text.len(), |length| url_start + length);

        output.push_str(&text[cursor..url_start]);

        let (redacted_url, url_redactions) =
            redact_url_remainder(&text[url_start..url_end], replacement);
        output.push_str(&redacted_url);
        redaction_count += url_redactions;

        cursor = url_end;
    }

    output.push_str(&text[cursor..]);

    (output, redaction_count)
}

fn has_scheme_before(prefix: &str) -> bool {
    let scheme: String = prefix
        .chars()
        .rev()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
        .collect();

    scheme
        .chars()
        .last()
        .is_some_and(|first| first.is_ascii_alphabetic())
}

fn is_url_terminator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '<' | '>')
}

/// Redacts one URL given everything after its `scheme://`.
fn redact_url_remainder(remainder: &str, replacement: &str) -> (String, usize) {
    let authority_end = remainder
        .find(['/', '?', '#', ';'])
        .unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let tail = &remainder[authority_end..];

    let mut redaction_count = 0;
    let mut output = String::with_capacity(remainder.len());

    match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, password)) if !password.is_empty() => {
                output.push_str(user);
                output.push(':');
                output.push_str(replacement);
                output.push('@');
                output.push_str(host);
                redaction_count += 1;
            }
            _ => output.push_str(authority),
        },
        None => output.push_str(authority),
    }

    let (redacted_tail, tail_redactions) = redact_sensitive_parameters(tail, replacement);
    output.push_str(&redacted_tail);
    redaction_count += tail_redactions;

    (output, redaction_count)
}

/// Replaces the values of sensitive `key=value` parameters that follow a `?`,
/// `&` or `;`, stopping each value at the next `&`, `;` or `#`.
fn redact_sensitive_parameters(tail: &str, replacement: &str) -> (String, usize) {
    let mut output = String::with_capacity(tail.len());
    let mut redaction_count = 0;
    let mut cursor = 0;

    while let Some(offset) = tail[cursor..].find(['?', '&', ';']) {
        let key_start = cursor + offset + 1;
        output.push_str(&tail[cursor..key_start]);
        cursor = key_start;

        let segment_end = tail[key_start..]
            .find(['&', ';', '#'])
            .map_or(tail.len(), |length| key_start + length);
        let segment = &tail[key_start..segment_end];

        let Some((key, value)) = segment.split_once('=') else {
            continue;
        };

        let is_sensitive = SENSITIVE_URI_PARAMETERS
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name));

        if is_sensitive && !value.is_empty() {
            output.push_str(key);
            output.push('=');
            output.push_str(replacement);
            redaction_count += 1;
            cursor = segment_end;
        }
    }

    output.push_str(&tail[cursor..]);

    (output, redaction_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_formatted_error_display() {
        let err = FormattedError::new("syntax error")
            .with_detail("near 'FROM'")
            .with_code("42601");

        assert_eq!(
            err.to_display_string(),
            "syntax error. Detail: near 'FROM'. Code: 42601"
        );
    }

    #[test]
    fn test_formatted_error_display_trait() {
        let err = FormattedError::new("test error");
        assert_eq!(format!("{}", err), "test error");
    }

    #[test]
    fn test_formatted_error_from_string() {
        let err: FormattedError = "hello".into();
        assert_eq!(err.message, "hello");

        let err: FormattedError = String::from("world").into();
        assert_eq!(err.message, "world");
    }

    #[test]
    fn test_formatted_error_with_location() {
        let err = FormattedError::new("duplicate key")
            .with_location(
                ErrorLocation::new()
                    .with_table("users")
                    .with_constraint("users_pkey"),
            )
            .with_code("23505");

        assert_eq!(
            err.to_display_string(),
            "duplicate key. Table: users. Constraint: users_pkey. Code: 23505"
        );
    }

    #[test]
    fn test_formatted_error_retriable() {
        let err = FormattedError::new("timeout").with_retriable(true);
        assert!(err.retriable);

        let err = FormattedError::new("syntax error");
        assert!(!err.retriable);
    }

    #[test]
    fn test_classify_constraint_violation() {
        let err = FormattedError::new("duplicate key").with_code("23505");
        match err.into_query_error() {
            DbError::ConstraintViolation(f) => assert_eq!(f.message, "duplicate key"),
            other => panic!("Expected ConstraintViolation, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_syntax_error() {
        let err = FormattedError::new("syntax error").with_code("42601");
        match err.into_query_error() {
            DbError::SyntaxError(f) => assert_eq!(f.message, "syntax error"),
            other => panic!("Expected SyntaxError, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_permission_denied() {
        let err = FormattedError::new("insufficient privilege").with_code("42501");
        match err.into_query_error() {
            DbError::PermissionDenied(f) => assert_eq!(f.message, "insufficient privilege"),
            other => panic!("Expected PermissionDenied, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_object_not_found() {
        let err = FormattedError::new("table not found").with_code("42P01");
        match err.into_query_error() {
            DbError::ObjectNotFound(f) => assert_eq!(f.message, "table not found"),
            other => panic!("Expected ObjectNotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_auth_failed() {
        let err = FormattedError::new("invalid password").with_code("28P01");
        match err.into_query_error() {
            DbError::AuthFailed(f) => assert_eq!(f.message, "invalid password"),
            other => panic!("Expected AuthFailed, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_auth_connection() {
        let err = FormattedError::new("invalid password").with_code("28P01");
        match err.into_connection_error() {
            DbError::AuthFailed(f) => assert_eq!(f.message, "invalid password"),
            other => panic!("Expected AuthFailed, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_no_code_defaults_to_query_failed() {
        let err = FormattedError::new("some error");
        match err.into_query_error() {
            DbError::QueryFailed(f) => assert_eq!(f.message, "some error"),
            other => panic!("Expected QueryFailed, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_mysql_not_found() {
        let err = FormattedError::new("table not found").with_code("1146");
        match err.into_query_error() {
            DbError::ObjectNotFound(f) => assert_eq!(f.message, "table not found"),
            other => panic!("Expected ObjectNotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_sanitize_uri_with_password() {
        let uri = "postgres://user:secret@localhost:5432/db";
        assert_eq!(sanitize_uri(uri), "postgres://user:***@localhost:5432/db");
    }

    #[test]
    fn sanitize_uri_masks_each_libpq_secret_query_key() {
        for key in [
            "sslpassword",
            "oauth_client_secret",
            "scram_client_key",
            "scram_server_key",
        ] {
            let output = sanitize_uri(&format!(
                "postgresql://db.example.com/db?{key}=hunter2&sslmode=require"
            ));
            assert!(!output.contains("hunter2"), "{key}: {output}");
            assert_eq!(
                output,
                format!("postgresql://db.example.com/db?{key}=***&sslmode=require")
            );
        }
    }

    #[test]
    fn uri_parameter_list_covers_every_libpq_secret_key() {
        for key in LIBPQ_SECRET_KEYS {
            assert!(
                SENSITIVE_URI_PARAMETERS.contains(&key),
                "{key} is missing from SENSITIVE_URI_PARAMETERS"
            );
        }
    }

    fn libpq(text: &str) -> String {
        sanitize_uri(&redact_libpq_secret_values(text, "***"))
    }

    fn assert_hides(input: &str, secrets: &[&str]) -> String {
        let output = libpq(input);
        for secret in secrets {
            assert!(
                !output.contains(secret),
                "{secret:?} shown in {output:?} for {input:?}"
            );
        }
        output
    }

    #[test]
    fn libpq_pass_masks_an_escaped_final_apostrophe_of_an_unclosed_value() {
        assert_eq!(
            assert_hides("postgresql://db.example.com/db?x='alpha\\'", &["alpha"]),
            "postgresql://db.example.com/db?x='***"
        );
        assert_eq!(
            assert_hides("host=db.example.com x='alpha\\'", &["alpha"]),
            "host=db.example.com x='***"
        );
        assert_eq!(libpq("host=h x='alpha'"), "host=h x='***'");
    }

    #[test]
    fn libpq_pass_masks_a_quoted_key() {
        assert_eq!(
            assert_hides("host=h 'password=hunter2'", &["hunter2"]),
            "host=h 'password=***"
        );
        assert_hides("host=h 'sslpassword=hunter2'", &["hunter2"]);
    }

    #[test]
    fn libpq_pass_masks_a_semicolon_separated_string() {
        assert_eq!(
            assert_hides("host=h;password=hunter2", &["hunter2"]),
            "host=***"
        );
        assert_hides("host=h;sslpassword=hunter2", &["hunter2"]);
    }

    #[test]
    fn libpq_pass_masks_keyword_text_after_a_url() {
        assert_eq!(
            assert_hides(
                "postgresql://db.example.com/db password=hunter2",
                &["hunter2"]
            ),
            "postgresql://db.example.com/db password=***"
        );
        assert_hides(
            "postgresql://db.example.com/db sslpassword=hunter2",
            &["hunter2"],
        );
    }

    #[test]
    fn libpq_pass_masks_oauth_and_scram_keys_and_spaces_around_equals() {
        for key in [
            "oauth_client_secret",
            "scram_client_key",
            "scram_server_key",
            "SSLPassword",
        ] {
            assert_hides(&format!("host=h {key}=hunter2"), &["hunter2"]);
            assert_hides(&format!("host=h {key} = hunter2"), &["hunter2"]);
        }
    }

    #[test]
    fn libpq_pass_masks_secrets_with_value_characters() {
        assert_hides("host=h password=hun;ter&2'x", &["hun", "ter", "2'x"]);
        assert_hides(
            "host=h password='hun;ter&2 \\' x' dbname=app",
            &["hun", "ter", "x'"],
        );
        assert_hides(
            "host=h password=hunter\\ two dbname=app",
            &["hunter", "two"],
        );
        assert_hides("host=h sslpassword=a;b&c'd", &["a;b", "b&c", "c'd"]);
    }

    #[test]
    fn libpq_pass_masks_every_value_to_whitespace_in_a_url() {
        for input in [
            "postgresql://db.example.com/db?sslpassword=alpha;bravo",
            "postgresql://db.example.com/db?sslpassword=alpha&bravo",
        ] {
            let output = assert_hides(input, &["alpha", "bravo"]);
            assert_eq!(output, "postgresql://db.example.com/db?sslpassword=***");
        }
    }

    #[test]
    fn libpq_pass_masks_a_password_that_looks_masked() {
        for input in [
            "postgresql://db.example.com/db?password=***;rest",
            "postgresql://db.example.com/db?password=***&rest",
            "host=h password=***;rest",
            "host=h password=***&rest dbname=a",
        ] {
            assert_hides(input, &["rest"]);
        }
    }

    #[test]
    fn libpq_pass_masks_a_misspelled_or_unknown_key() {
        assert_hides("host=h pasword=hunter2", &["hunter2"]);
        assert_hides(
            "postgresql://db.example.com/db?sslmode=require&pasword=hunter2",
            &["hunter2"],
        );
        assert_hides(
            "postgresql://db.example.com/db?sslmode=require;pasword=hunter2",
            &["hunter2"],
        );
        assert_hides("host=h xpassword=hunter2", &["hunter2"]);
        assert_hides("host=h my_password = hunter2", &["hunter2"]);
    }

    #[test]
    fn libpq_pass_keeps_public_keys_readable() {
        for input in [
            "host=h port=5432 sslmode=require",
            "host=h sslkey=k.pem passfile=pgpass sslkeylogfile=/tmp/keys",
            "host=h dbname='my db' user = app",
        ] {
            assert_eq!(libpq(input), input);
        }
        assert_eq!(
            libpq("postgresql://db.example.com/db?sslmode=require&connect_timeout=5"),
            "postgresql://db.example.com/db?sslmode=***"
        );
        assert_eq!(
            libpq("host=h sslkey=k.pem passfile=pgpass password=hunter2"),
            "host=h sslkey=k.pem passfile=pgpass password=***"
        );
    }

    #[test]
    fn userinfo_passwords_are_masked_up_to_the_last_at() {
        for input in [
            "postgresql://u:alpha&password=x@db.example.com/db",
            "postgresql://u:alpha;bravo@db.example.com/db",
            "see postgresql://u:alpha@bravo.example.com@db.example.com/db?sslmode=require now",
        ] {
            let output = redact_userinfo_passwords(input, "***");
            for secret in ["alpha", "bravo", "x@"] {
                assert!(!output.contains(secret), "{secret:?} in {output}");
            }
            assert!(output.contains("u:***@db.example.com/db"), "{output}");
        }
        for input in [
            "postgresql://u:p@db.example.com/db?password=alpha@bravo.example.com",
            "postgresql://u:p@db.example.com/db?options=-c%20app.api_token%3Dalpha@bravo.example.com",
        ] {
            let output = redact_userinfo_passwords(input, "***");
            assert_eq!(output, "postgresql://u:***", "{output}");
        }
        assert_eq!(
            redact_userinfo_passwords("postgresql://u:secret@db.example.com/db", "***"),
            "postgresql://u:***@db.example.com/db"
        );
        assert_eq!(
            redact_userinfo_passwords("postgresql://u@db.example.com/db", "***"),
            "postgresql://u@db.example.com/db"
        );
        assert_eq!(
            redact_userinfo_passwords("postgresql://db.example.com/db", "***"),
            "postgresql://db.example.com/db"
        );
    }

    #[test]
    fn libpq_pass_masks_a_public_value_that_holds_a_secret_key() {
        let output = assert_hides(
            r"host=h application_name='password=\'alpha bravo\'' port=5432",
            &["alpha", "bravo"],
        );
        assert_eq!(output, "host=h application_name='***' port=5432");
        assert_hides(
            r"application_name='x pasword=\'alpha bravo\' y' dbname=app",
            &["alpha", "bravo"],
        );
        assert_hides(
            r"application_name='password=\'alpha bravo\'",
            &["alpha", "bravo"],
        );
        assert_eq!(
            libpq("application_name='my app' dbname='a b'"),
            "application_name='my app' dbname='a b'"
        );
    }

    #[test]
    fn libpq_pass_masks_non_ascii_keys() {
        let output = assert_hides(
            "postgresql://u@db.example.com/db?密码=hunter2&sslmode=require",
            &["hunter2"],
        );
        assert!(output.contains("u@db.example.com/db?密码=***"), "{output}");
        assert_hides("host=h 密码=hunter2", &["hunter2"]);
        assert_hides("host=h pässword=hunter2 port=1", &["hunter2"]);
        assert_hides(
            "postgresql://db.example.com/db?密码=alpha@bravo.example.com",
            &["alpha", "bravo"],
        );
    }

    #[test]
    fn libpq_pass_masks_any_key_that_is_not_public() {
        for input in [
            "postgresql://db.example.com/db?🔑=hunter2",
            "postgresql://db.example.com/db?x\u{301}=hunter2",
            "postgresql://db.example.com/db?x-host=hunter2",
            "postgresql://db.example.com/db?-=hunter2",
            "postgresql://db.example.com/db?host-port=hunter2",
            "postgresql://u@db.example.com/db?🔑=alpha@bravo.example.com",
            "host=h application_name=password='alpha'bravo",
        ] {
            assert_hides(input, &["hunter2", "alpha", "bravo"]);
        }
    }

    #[test]
    fn libpq_pass_shows_a_public_value_only_when_it_holds_no_equals_sign() {
        assert_eq!(
            libpq("postgresql://db.example.com/db?sslmode=require&password=x"),
            "postgresql://db.example.com/db?sslmode=***"
        );
        let readable = "host=h port=5432 sslmode=require";
        assert_eq!(libpq(readable), readable);
    }

    #[test]
    fn libpq_pass_masks_options() {
        assert_hides("host=h options='-c app.api_token=hunter2'", &["hunter2"]);
        assert_hides(
            "postgresql://db.example.com/db?options=-c%20app.api_token%3Dhunter2",
            &["hunter2"],
        );
    }

    #[test]
    fn secret_and_public_libpq_key_lists_do_not_overlap() {
        for key in LIBPQ_SECRET_KEYS {
            assert!(!is_libpq_public_key(key), "{key} is secret");
        }
    }

    #[test]
    fn test_sanitize_uri_without_password() {
        let uri = "postgres://localhost:5432/db";
        assert_eq!(sanitize_uri(uri), "postgres://localhost:5432/db");
    }

    fn redact(text: &str) -> String {
        redact_uri_credentials(text, "***").0
    }

    #[test]
    fn redacts_userinfo_password_for_every_scheme() {
        let cases = [
            (
                "postgres://u:pw@h.example.com:5432/db",
                "postgres://u:***@h.example.com:5432/db",
            ),
            (
                "postgresql://u:pw@h.example.com:5432/db",
                "postgresql://u:***@h.example.com:5432/db",
            ),
            (
                "mysql://root:pw@h.example.com:3306/app",
                "mysql://root:***@h.example.com:3306/app",
            ),
            (
                "mariadb://root:pw@h.example.com/app",
                "mariadb://root:***@h.example.com/app",
            ),
            (
                "mongodb://u:pw@h1.example.com:27017,h2.example.com:27017/db",
                "mongodb://u:***@h1.example.com:27017,h2.example.com:27017/db",
            ),
            (
                "mongodb+srv://u:pw@cluster.example.net/db",
                "mongodb+srv://u:***@cluster.example.net/db",
            ),
            (
                "redis://:pw@h.example.com:6379/0",
                "redis://:***@h.example.com:6379/0",
            ),
            (
                "rediss://default:pw@h.example.com:6380",
                "rediss://default:***@h.example.com:6380",
            ),
            (
                "sqlserver://sa:pw@h.example.com:1433/master",
                "sqlserver://sa:***@h.example.com:1433/master",
            ),
            (
                "clickhouse://u:pw@h.example.com:8123",
                "clickhouse://u:***@h.example.com:8123",
            ),
            (
                "http://u:pw@h.example.com:8086/api",
                "http://u:***@h.example.com:8086/api",
            ),
            (
                "https://u:pw@h.example.com/path",
                "https://u:***@h.example.com/path",
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(redact(input), expected, "input: {input}");
        }
    }

    #[test]
    fn leaves_uris_without_password_unchanged() {
        for input in [
            "postgres://u@h.example.com:5432/db",
            "postgresql://db.example.com/db",
            "mongodb://h:27017/db?replicaSet=rs0",
            "redis://[::1]:6379",
            "https://example.com/a@b",
        ] {
            let (output, count) = redact_uri_credentials(input, "***");
            assert_eq!(output, input);
            assert_eq!(count, 0, "input: {input}");
        }
    }

    #[test]
    fn redacts_password_with_encoded_or_raw_at_sign() {
        assert_eq!(
            redact("postgresql://alice:p%40ss@localhost/app"),
            "postgresql://alice:***@localhost/app"
        );
        assert_eq!(
            redact("postgresql://alice:p@example.com:1234@localhost/app"),
            "postgresql://alice:***@localhost/app"
        );
    }

    #[test]
    fn redacts_password_before_ipv6_host() {
        assert_eq!(
            redact("postgresql://u:pw@[2001:db8::1]:5432/db"),
            "postgresql://u:***@[2001:db8::1]:5432/db"
        );
        assert_eq!(redact("redis://:pw@[::1]:6379"), "redis://:***@[::1]:6379");
    }

    #[test]
    fn redacts_sensitive_query_and_jdbc_parameters() {
        assert_eq!(
            redact("postgresql://db.example.com/db?user=u&password=pw&sslmode=require"),
            "postgresql://db.example.com/db?user=u&password=***&sslmode=require"
        );
        assert_eq!(
            redact("https://h/api?Token=abc#frag"),
            "https://h/api?Token=***#frag"
        );
        assert_eq!(
            redact("mysql://h/db?PWD=x&secret=y"),
            "mysql://h/db?PWD=***&secret=***"
        );
        assert_eq!(
            redact("jdbc:sqlserver://h:1433;user=sa;password=pw;databaseName=app"),
            "jdbc:sqlserver://h:1433;user=sa;password=***;databaseName=app"
        );
    }

    #[test]
    fn redacts_urls_inside_free_text() {
        let (output, count) = redact_uri_credentials(
            "Failed to connect using URI postgresql://u:pw@h.example.com:5432/db: refused; retry mysql://r:x@h2.example.com/db",
            "[REDACTED]",
        );

        assert_eq!(
            output,
            "Failed to connect using URI postgresql://u:[REDACTED]@h.example.com:5432/db: refused; retry mysql://r:[REDACTED]@h2.example.com/db"
        );
        assert_eq!(count, 2);
    }

    #[test]
    fn stops_at_quotes_and_ignores_schemeless_separators() {
        assert_eq!(
            redact(r#"{"uri":"redis://u:pw@h.example.com:6379","note":"a@b.example.com"}"#),
            r#"{"uri":"redis://u:***@h.example.com:6379","note":"a@b.example.com"}"#
        );
        assert_eq!(redact("://u:pw@h.example.com"), "://u:pw@h.example.com");
    }

    #[test]
    fn test_error_location_is_empty() {
        assert!(ErrorLocation::new().is_empty());
        assert!(!ErrorLocation::new().with_table("users").is_empty());
    }

    /// A server that counts the first character as 1 must land on 0, so the
    /// editor's cursor sits on the character the server named.
    #[test]
    fn one_based_character_position_loses_its_leading_one() {
        assert_eq!(
            ErrorPosition::from_one_based_character(1).character_offset,
            Some(0)
        );
        assert_eq!(
            ErrorPosition::from_one_based_character(8).character_offset,
            Some(7)
        );
    }

    /// A server that sends no position (0) must not be read as "the start of
    /// the statement".
    #[test]
    fn zero_one_based_position_reports_nothing() {
        assert_eq!(
            ErrorPosition::from_one_based_character(0).character_offset,
            None
        );
        assert_eq!(ErrorPosition::from_one_based_character(0).line, None);
    }

    /// SQLite counts bytes, so an offset after a multibyte character must
    /// become the character count, not stay a byte count.
    #[test]
    fn byte_offset_becomes_a_character_offset() {
        let statement = "SELECT 'é' FROM";
        // "SELECT '" is 8 bytes and 8 characters; the 'é' starts at byte 8.
        assert_eq!(
            ErrorPosition::from_byte_offset(statement, 8).character_offset,
            Some(8),
            "a byte offset inside ASCII is also its character offset"
        );
        // The closing quote sits after the 2-byte 'é'.
        assert_eq!(
            ErrorPosition::from_byte_offset(statement, 10).character_offset,
            Some(9),
            "a byte offset past a multibyte character counts as one character"
        );
    }

    /// An offset a driver got wrong must clamp, never slice past the end.
    #[test]
    fn byte_offset_past_the_end_clamps() {
        assert_eq!(
            ErrorPosition::from_byte_offset("SELECT 1", 400).character_offset,
            Some(8)
        );
        assert_eq!(
            ErrorPosition::from_byte_offset("SELECT 'é'", 9).character_offset,
            Some(8),
            "an offset inside a multibyte character moves back to its start"
        );
    }

    #[test]
    fn line_only_position_resolves_to_the_start_of_that_line() {
        let statement = "SELECT 1;\nSELECT 2;\nSELECT 3;";
        let position = ErrorPosition::from_line(2);

        assert_eq!(position.resolve_character_offset(statement), Some(10));
        assert_eq!(
            ErrorPosition::from_line(0).resolve_character_offset(statement),
            None,
            "line 0 has nothing to report"
        );
    }

    #[test]
    fn a_position_past_the_end_resolves_to_the_end() {
        let statement = "SELECT 1";
        let past_end = ErrorPosition {
            character_offset: Some(400),
            line: None,
        };
        assert_eq!(past_end.resolve_character_offset(statement), Some(8));

        let line_past_the_end = ErrorPosition::from_line(9);
        assert_eq!(
            line_past_the_end.resolve_character_offset(statement),
            Some(8)
        );
    }

    #[test]
    fn a_driver_that_reports_no_position_resolves_to_nothing() {
        assert_eq!(
            ErrorPosition::default().resolve_character_offset("SELECT 1"),
            None
        );
        assert_eq!(FormattedError::new("boom").position, None);
    }

    /// An offset is more precise than a line, so it wins when a driver gives
    /// both.
    #[test]
    fn an_offset_wins_over_a_line() {
        let position = ErrorPosition {
            character_offset: Some(3),
            line: Some(2),
        };
        assert_eq!(
            position.resolve_character_offset("SELECT 1;\nSELECT 2;"),
            Some(3)
        );
    }
}
