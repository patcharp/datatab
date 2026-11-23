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
        let (raw_value, remaining) = rest.split_at(datatab_core::libpq_value_end(rest));
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

const REDACTED_VALUE: &str = "***";
const HIDDEN_STRING: &str = "[connection string hidden]";

/// Renders a keyword string for an error or a log line. Only the value of a
/// public libpq key (`datatab_core::LIBPQ_PUBLIC_KEYS`) stays; every other
/// key, a misspelled one included, shows as `key=***`.
pub(super) fn redacted(input: &str) -> String {
    let Ok(parameters) = parameters(input) else {
        // Without parameter boundaries no part of the string is safe to show.
        return HIDDEN_STRING.to_string();
    };
    parameters
        .iter()
        .map(|parameter| {
            if datatab_core::is_libpq_public_key(parameter.key) {
                parameter.raw.to_string()
            } else {
                format!("{}={REDACTED_VALUE}", parameter.key)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn invalid_keywords() -> DbError {
    // Never include a keyword string or a wire-parser error: it may hold secrets.
    DbError::InvalidProfile("Invalid libpq keyword connection string".to_string())
}
