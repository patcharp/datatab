//! Structured error formatting for CQL errors.
//!
//! Cassandra answers a failed request with a numeric code and a reason. The
//! code is the server's own, so it travels in [`FormattedError::code`] and a
//! reader can look it up; the reason is the message.

use std::error::Error;

use datatab_core::{ConnectionErrorFormatter, DbError, FormattedError, QueryErrorFormatter};
use scylla::errors::{
    ConnectionError, ConnectionPoolError, ConnectionSetupRequestErrorKind, DbError as CqlDbError,
    ExecutionError, MetadataError, NewSessionError, RequestAttemptError, UseKeyspaceError,
};

pub struct CassandraErrorFormatter;

impl CassandraErrorFormatter {
    /// Maps a failed statement to the matching `DbError` variant.
    pub(crate) fn into_query_error(error: &(dyn Error + 'static)) -> DbError {
        let formatted = Self::format_any(error);
        if is_timeout(error) {
            return DbError::Timeout;
        }
        match cql_error(error) {
            Some(CqlDbError::SyntaxError) => DbError::SyntaxError(formatted),
            Some(CqlDbError::Unauthorized) => DbError::PermissionDenied(formatted),
            Some(CqlDbError::AuthenticationError) => DbError::AuthFailed(formatted),
            Some(CqlDbError::Invalid) if names_a_missing_object(&formatted.message) => {
                DbError::ObjectNotFound(formatted)
            }
            _ => DbError::QueryFailed(formatted),
        }
    }

    /// Maps a failed session build to the matching `DbError` variant.
    ///
    /// A connect fails inside the crate's own chain, not with a CQL error at the
    /// top: the server's answer arrives as
    /// `NewSessionError::MetadataError(..)` →
    /// `ConnectionPoolError::Broken { last_connection_error }` →
    /// `ConnectionError::ConnectionSetupRequestError(..)`, and only the last
    /// link's kind carries the `DbError` the server sent. That chain is
    /// unwrapped by its typed variants here, so a refused password comes out as
    /// `AuthFailed` instead of a generic connection failure. The query path's
    /// own `cql_error` below still covers an answer that reached a session.
    pub(crate) fn into_connection_error(error: &(dyn Error + 'static)) -> DbError {
        let formatted =
            <Self as ConnectionErrorFormatter>::format_connection_error(&Self, error, "", 0);
        if let Some(refusal) = connection_setup_cql_error(error) {
            return Self::connection_refusal(refusal, formatted);
        }
        match cql_error(error) {
            Some(CqlDbError::AuthenticationError) => DbError::AuthFailed(formatted),
            Some(CqlDbError::Unauthorized) => DbError::PermissionDenied(formatted),
            _ => DbError::ConnectionFailed(formatted),
        }
    }

    /// What a CQL answer to a connection setup request means to the user.
    ///
    /// The cluster would not authenticate this profile, or would not set a
    /// session up for it: either way there is no statement to be unauthorized
    /// for, and the profile cannot be used. The server's own reason travels in
    /// the body.
    fn connection_refusal(refusal: &CqlDbError, formatted: FormattedError) -> DbError {
        match refusal {
            CqlDbError::AuthenticationError | CqlDbError::Unauthorized => {
                DbError::AuthFailed(formatted)
            }
            _ => DbError::ConnectionFailed(formatted),
        }
    }

    fn format_any(error: &(dyn Error + 'static)) -> FormattedError {
        if is_timeout(error) {
            return FormattedError::new(
                "The cluster did not answer within the driver's request timeout",
            )
            .with_hint("Retry the statement, or check whether a node is overloaded")
            .with_retriable(true);
        }

        let (code, reason) = match server_parts(error) {
            Some(parts) => parts,
            None => return FormattedError::new(error.to_string()),
        };
        let retriable = cql_error(error).is_some_and(is_retriable);
        match code {
            Some(code) => FormattedError::new(reason)
                .with_code(code)
                .with_retriable(retriable),
            None => FormattedError::new(reason).with_retriable(retriable),
        }
    }
}

/// The server's own error, when the failure came from the server rather than
/// from the driver or the network.
fn cql_error<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a CqlDbError> {
    match error.downcast_ref::<ExecutionError>() {
        Some(ExecutionError::LastAttemptError(RequestAttemptError::DbError(db_error, _))) => {
            Some(db_error)
        }
        _ => None,
    }
}

/// The CQL error the server answered a connection setup request with, when the
/// failure came from the crate's setup chain.
///
/// Every step is a typed variant. The walk stops at
/// `ConnectionSetupRequestError`, whose kind the crate exposes through
/// `get_error()` and closes to construction, so `setup_kind_cql_error` reads it
/// there.
fn connection_setup_cql_error<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a CqlDbError> {
    if let Some(session) = error.downcast_ref::<NewSessionError>() {
        return match session {
            NewSessionError::MetadataError(MetadataError::ConnectionPoolError(pool)) => {
                pool_cql_error(pool)
            }
            NewSessionError::UseKeyspaceError(keyspace) => keyspace_cql_error(keyspace),
            _ => None,
        };
    }
    error
        .downcast_ref::<UseKeyspaceError>()
        .and_then(keyspace_cql_error)
}

/// The CQL error a broken connection pool reports, taken from the last
/// connection it tried.
fn pool_cql_error(pool: &ConnectionPoolError) -> Option<&CqlDbError> {
    match pool {
        ConnectionPoolError::Broken {
            last_connection_error: ConnectionError::ConnectionSetupRequestError(setup),
        } => setup_kind_cql_error(setup.get_error()),
        _ => None,
    }
}

/// The CQL error a failed `USE KEYSPACE` reports.
fn keyspace_cql_error(error: &UseKeyspaceError) -> Option<&CqlDbError> {
    match error {
        UseKeyspaceError::RequestError(RequestAttemptError::DbError(db_error, _)) => Some(db_error),
        _ => None,
    }
}

/// The CQL error inside a failed setup request's kind.
fn setup_kind_cql_error(kind: &ConnectionSetupRequestErrorKind) -> Option<&CqlDbError> {
    match kind {
        ConnectionSetupRequestErrorKind::DbError(db_error, _) => Some(db_error),
        _ => None,
    }
}

fn is_timeout(error: &(dyn Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<ExecutionError>(),
        Some(ExecutionError::RequestTimeout(_))
    )
}

/// Whether a CQL "invalid" reason means the object is missing rather than
/// malformed.
///
/// Cassandra words it `unconfigured table x` or `unconfigured keyspace x`, and
/// Cassandra 5.0 and ScyllaDB 6.2 word it `<kind> x does not exist` — for a
/// table, a keyspace, a column or a function; the live suites confirm both
/// servers answer it that way. Either spelling means the same thing to the user:
/// the object is not there, so the tree can offer to create it. The older
/// phrasings stay, since a cluster does not change its wording with a driver
/// release.
fn names_a_missing_object(reason: &str) -> bool {
    reason.contains("unconfigured table")
        || reason.contains("unconfigured keyspace")
        || reason.contains("Undefined column name")
        || reason.contains("does not exist")
}

/// The numeric CQL code, when this build names it, and the server's reason.
fn server_parts(error: &(dyn Error + 'static)) -> Option<(Option<String>, String)> {
    match error.downcast_ref::<ExecutionError>() {
        Some(ExecutionError::LastAttemptError(RequestAttemptError::DbError(db_error, reason))) => {
            Some((cql_code(db_error), reason.clone()))
        }
        _ => None,
    }
}

/// The wire code of a server error.
///
/// `CqlDbError` is `#[non_exhaustive]`, so a variant a newer protocol adds
/// reaches the catch-all and simply travels without a code.
fn cql_code(db_error: &CqlDbError) -> Option<String> {
    let code: u32 = match db_error {
        CqlDbError::ServerError => 0x0000,
        CqlDbError::ProtocolError => 0x000A,
        CqlDbError::AuthenticationError => 0x0100,
        CqlDbError::Unavailable { .. } => 0x1000,
        CqlDbError::Overloaded => 0x1001,
        CqlDbError::IsBootstrapping => 0x1002,
        CqlDbError::TruncateError => 0x1003,
        CqlDbError::WriteTimeout { .. } => 0x1100,
        CqlDbError::ReadTimeout { .. } => 0x1200,
        CqlDbError::ReadFailure { .. } => 0x1300,
        CqlDbError::FunctionFailure { .. } => 0x1400,
        CqlDbError::WriteFailure { .. } => 0x1500,
        CqlDbError::SyntaxError => 0x2000,
        CqlDbError::Unauthorized => 0x2100,
        CqlDbError::Invalid => 0x2200,
        CqlDbError::ConfigError => 0x2300,
        CqlDbError::AlreadyExists { .. } => 0x2400,
        CqlDbError::Unprepared { .. } => 0x2500,
        CqlDbError::Other(code) => return Some(format!("0x{code:04X}")),
        // Includes Scylla's rate-limit code, whose number depends on the
        // negotiated protocol features this formatter does not hold.
        _ => return None,
    };
    Some(format!("0x{code:04X}"))
}

/// True for the codes that mean "the cluster could not answer right now".
fn is_retriable(db_error: &CqlDbError) -> bool {
    matches!(
        db_error,
        CqlDbError::Overloaded
            | CqlDbError::IsBootstrapping
            | CqlDbError::Unavailable { .. }
            | CqlDbError::ReadTimeout { .. }
            | CqlDbError::WriteTimeout { .. }
            | CqlDbError::ReadFailure { .. }
            | CqlDbError::WriteFailure { .. }
            | CqlDbError::RateLimitReached { .. }
    )
}

impl QueryErrorFormatter for CassandraErrorFormatter {
    fn format_query_error(&self, error: &(dyn Error + 'static)) -> FormattedError {
        Self::format_any(error)
    }
}

impl ConnectionErrorFormatter for CassandraErrorFormatter {
    fn format_connection_error(
        &self,
        error: &(dyn Error + 'static),
        host: &str,
        port: u16,
    ) -> FormattedError {
        let formatted = Self::format_any(error);
        if host.is_empty() {
            return formatted;
        }
        formatted.with_hint(format!(
            "Check that the contact point {host}:{port} is reachable and speaks CQL"
        ))
    }

    fn format_uri_error(
        &self,
        error: &(dyn Error + 'static),
        sanitized_uri: &str,
    ) -> FormattedError {
        Self::format_any(error).with_detail(format!("Contact point: {sanitized_uri}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{CassandraErrorFormatter, connection_setup_cql_error, setup_kind_cql_error};
    use datatab_core::{DbError, FormattedError};
    use scylla::errors::{
        ConnectionSetupRequestErrorKind, DbError as CqlDbError, ExecutionError, NewSessionError,
        RequestAttemptError, UseKeyspaceError,
    };

    /// The body a failed connect carries, built as the formatter builds it.
    fn body() -> FormattedError {
        FormattedError::new("Failed to perform a connection setup request")
    }

    #[test]
    fn an_authentication_failure_at_connect_is_auth_failed() {
        // The shape the server's bad-credentials answer arrives in: the setup
        // request's kind, carrying the CQL error and the server's message.
        let kind = ConnectionSetupRequestErrorKind::DbError(
            CqlDbError::AuthenticationError,
            "Password must not be null".to_string(),
        );
        let refusal = setup_kind_cql_error(&kind).expect("the kind carries the server's answer");
        let mapped = CassandraErrorFormatter::connection_refusal(refusal, body());
        assert!(matches!(mapped, DbError::AuthFailed(_)), "got {mapped:?}");
    }

    #[test]
    fn a_refused_identity_at_connect_is_auth_failed_too() {
        // An authenticated role the cluster will not set a session up for has
        // no statement to be unauthorized for, so the user is told the same
        // thing as a refused password.
        let kind = ConnectionSetupRequestErrorKind::DbError(
            CqlDbError::Unauthorized,
            "User has no permission to connect".to_string(),
        );
        let refusal = setup_kind_cql_error(&kind).expect("the kind carries the server's answer");
        let mapped = CassandraErrorFormatter::connection_refusal(refusal, body());
        assert!(matches!(mapped, DbError::AuthFailed(_)), "got {mapped:?}");
    }

    #[test]
    fn another_setup_failure_stays_a_connection_failure() {
        let kind = ConnectionSetupRequestErrorKind::UnableToAllocStreamId;
        assert!(
            setup_kind_cql_error(&kind).is_none(),
            "a setup failure the server never answered carries no CQL error"
        );
        let mapped = CassandraErrorFormatter::connection_refusal(&CqlDbError::Overloaded, body());
        assert!(
            matches!(mapped, DbError::ConnectionFailed(_)),
            "an answer that is not about the identity is not an auth failure: {mapped:?}"
        );
    }

    #[test]
    fn a_refused_use_keyspace_at_connect_is_auth_failed() {
        // The one link of the chain an outside test can build whole: the error
        // the session builder returns when its `USE KEYSPACE` is refused. It is
        // read by the same walk, and the query path's `cql_error` does not see
        // it, so this fails if the walk is taken out.
        let error = UseKeyspaceError::RequestError(RequestAttemptError::DbError(
            CqlDbError::AuthenticationError,
            "Password must not be null".to_string(),
        ));
        let mapped = CassandraErrorFormatter::into_connection_error(&error);
        assert!(matches!(mapped, DbError::AuthFailed(_)), "got {mapped:?}");
    }

    /// The failure shape the query path downcasts: the last attempt's answer is
    /// a CQL error and the server's own reason.
    fn invalid_query(reason: &str) -> ExecutionError {
        ExecutionError::LastAttemptError(RequestAttemptError::DbError(
            CqlDbError::Invalid,
            reason.to_string(),
        ))
    }

    #[test]
    fn a_missing_object_in_cassandras_wording_is_object_not_found() {
        // The exact message the local Cassandra 5.0 answered `SELECT * FROM
        // datatab_live.not_a_table` with, and the reason the suite failed
        // before this mapping learned it.
        let mapped = CassandraErrorFormatter::into_query_error(&invalid_query(
            "table not_a_table does not exist",
        ));
        assert!(
            matches!(mapped, DbError::ObjectNotFound(_)),
            "got {mapped:?}"
        );
    }

    #[test]
    fn a_plain_invalid_query_stays_a_query_failure() {
        // `does not exist` must not be read into every invalid query: this one
        // is a malformed statement, from the server's own parser.
        let mapped = CassandraErrorFormatter::into_query_error(&invalid_query(
            "line 1:5 mismatched input 'FROM' expecting EOF",
        ));
        assert!(matches!(mapped, DbError::QueryFailed(_)), "got {mapped:?}");
    }

    #[test]
    fn an_error_from_another_path_is_not_read_as_a_setup_failure() {
        let unrelated = NewSessionError::EmptyKnownNodesList;
        assert!(connection_setup_cql_error(&unrelated).is_none());
    }
}
