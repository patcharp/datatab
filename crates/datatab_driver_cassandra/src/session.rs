//! Session construction and the Tokio bridge that drives it.
//!
//! The `scylla` crate is async. Driver methods are synchronous, so one
//! runtime per profile drives every future the connection and its session
//! produce.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use datatab_core::{DbConfig, DbError};
use scylla::client::session::Session;
use scylla::client::session_builder::SessionBuilder;

use crate::error_formatter::CassandraErrorFormatter;
use crate::tls::{self, TlsMode};

/// How long a node is given to answer before the session reports the node
/// down. The control connection and every pool use it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Owns the Tokio runtime that drives the session's futures for one profile.
pub(crate) struct Worker {
    runtime: tokio::runtime::Runtime,
}

impl Worker {
    pub(crate) fn new() -> Result<Self, DbError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("datatab-cassandra-io")
            .enable_all()
            .build()
            .map_err(|error| {
                DbError::connection_failed(format!(
                    "Failed to start the Cassandra I/O runtime: {error}"
                ))
            })?;
        Ok(Self { runtime })
    }

    /// Drives a future to completion from synchronous code.
    ///
    /// Driver methods run on plain background threads, where `block_on` is
    /// fine. When a caller is already inside a Tokio context (the MCP server
    /// runs tools under `spawn_blocking`), nesting `block_on` would panic, so
    /// the future is driven from a scoped helper thread instead.
    pub(crate) fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::scope(|scope| {
                let handle = scope.spawn(|| self.runtime.block_on(future));
                match handle.join() {
                    Ok(output) => output,
                    Err(payload) => std::panic::resume_unwind(payload),
                }
            })
        } else {
            self.runtime.block_on(future)
        }
    }
}

/// Opens one session for a profile.
///
/// The worker comes back with the session because the session's connections
/// are driven by that runtime for the session's whole life.
pub(crate) fn open_session(
    config: &DbConfig,
    password: Option<&str>,
) -> Result<(Arc<Worker>, Session), DbError> {
    let DbConfig::Cassandra {
        contact_points,
        port,
        datacenter,
        keyspace,
        user,
        ssl_mode,
        ssl_root_cert_path,
        ssl_client_cert_path,
        ssl_client_key_path,
        ..
    } = config
    else {
        return Err(DbError::InvalidProfile(
            "Profile is not a Cassandra configuration".into(),
        ));
    };

    let hosts: Vec<&str> = contact_points
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .collect();
    if hosts.is_empty() {
        return Err(DbError::InvalidProfile(
            "At least one contact point is required".into(),
        ));
    }

    let mode = match ssl_mode.as_deref().map(str::trim) {
        None | Some("") => TlsMode::Disable,
        Some(id) => TlsMode::parse(id)?,
    };
    let tls_config = tls::client_config(
        mode,
        ssl_root_cert_path.as_deref(),
        ssl_client_cert_path.as_deref(),
        ssl_client_key_path.as_deref(),
    )?;

    let mut builder = SessionBuilder::new().connection_timeout(CONNECT_TIMEOUT);
    for host in &hosts {
        // A host name is resolved by the driver; an IP address is used as it
        // is. Both take the port from the profile's one Port field.
        builder = builder.known_node(node_address(host, *port));
    }
    if !datacenter.trim().is_empty() {
        builder = builder.prefer_datacenter(datacenter.trim().to_string());
    }
    if !keyspace.trim().is_empty() {
        // The keyspace is used as written: quoting keeps a mixed-case name
        // intact instead of folding it to lower case.
        builder = builder.use_keyspace(keyspace.trim().to_string(), true);
    }
    if !user.trim().is_empty() {
        // The driver takes the password inline; it lives in memory for the
        // life of the session and is never logged.
        builder = builder.user(user.trim().to_string(), password.unwrap_or_default());
    }
    if let Some(client_config) = tls_config {
        builder = builder.tls_context(Some(scylla::client::session::TlsContext::Rustls023(
            client_config,
        )));
    }

    let worker = Arc::new(Worker::new()?);
    let session = worker
        .block_on(builder.build())
        .map_err(|error| CassandraErrorFormatter::into_connection_error(&error))?;
    Ok((worker, session))
}

/// The `host:port` form the crate parses. An IPv6 address needs its brackets:
/// without them the crate reads the address as a host name and the lookup fails.
pub(crate) fn node_address(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
