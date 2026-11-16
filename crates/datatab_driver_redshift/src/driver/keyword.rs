//! Locate keyword parameters without mistaking quoted text for TLS settings,
//! and mask their secrets for error text and logs.
//! Twin: `datatab_driver_postgres/src/driver/keyword.rs`; keep both in sync.

use datatab_core::DbError;

pub(super) struct Parameter<'a> {
    pub key: &'a str,
    pub raw: &'a str,
    pub value: String,
}

pub(super) fn parameters(mut input: &str) -> Result<Vec<Parameter<'_>>, DbError> {
    let mut parameters = Vec::new();
    while !input.trim_start().is_empty() {
        input = input.trim_start();
        let start = input;
        let key_end = input
            .find(|character: char| character == '=' || character.is_whitespace())
            .unwrap_or(input.len());
        let (key, rest) = input.split_at(key_end);
        let rest = rest
            .trim_start()
            .strip_prefix('=')
            .ok_or_else(invalid_keywords)?;
        if key.is_empty() {
            return Err(invalid_keywords());
        }
        let rest = rest.trim_start();
        let (raw_value, remaining) = rest.split_at(value_end(rest));
        // Let the wire parser decode libpq quotes and backslash escapes. The
        // neutral key accepts all values, including the driver's verify modes.
        let decoded = format!("application_name={raw_value}")
            .parse::<postgres::Config>()
            .map_err(|_| invalid_keywords())?;
        let value = decoded
            .get_application_name()
            .ok_or_else(invalid_keywords)?
            .to_string();
        let (raw, _) = start.split_at(start.len() - remaining.len());
        parameters.push(Parameter { key, raw, value });
        input = remaining;
    }
    Ok(parameters)
}

/// libpq keys whose value is itself a secret. `passfile`, `sslkey` and
/// `sslkeylogfile` hold paths, so they stay readable.
const SECRET_KEYS: [&str; 5] = [
    "password",
    "sslpassword",
    "oauth_client_secret",
    "scram_client_key",
    "scram_server_key",
];

const REDACTED_VALUE: &str = "***";
const HIDDEN_STRING: &str = "[connection string hidden]";

/// Renders a keyword string for an error or a log line, with every secret
/// value replaced.
pub(super) fn redacted(input: &str) -> String {
    let Ok(parameters) = parameters(input) else {
        // Without parameter boundaries no part of the string is safe to show.
        return HIDDEN_STRING.to_string();
    };
    parameters
        .iter()
        .map(|parameter| {
            if is_secret_key(parameter.key) {
                format!("{}={REDACTED_VALUE}", parameter.key)
            } else {
                parameter.raw.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS
        .iter()
        .any(|secret_key| key.eq_ignore_ascii_case(secret_key))
}

fn value_end(input: &str) -> usize {
    let quoted = input.starts_with('\'');
    let mut escaped = false;
    for (offset, character) in input.char_indices().skip(usize::from(quoted)) {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if quoted && character == '\'' {
            return offset + character.len_utf8();
        } else if !quoted && character.is_whitespace() {
            return offset;
        }
    }
    input.len()
}

fn invalid_keywords() -> DbError {
    // Never include a keyword string or a wire-parser error: it may hold secrets.
    DbError::InvalidProfile("Invalid libpq keyword connection string".to_string())
}
