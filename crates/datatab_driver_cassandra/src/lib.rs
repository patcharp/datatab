//! Cassandra and ScyllaDB driver over CQL.
//!
//! Speaks CQL through the `scylla` crate. The crate is async-only while
//! DataTab drivers are synchronous, so every connection owns a small Tokio
//! runtime and bridges calls with `block_on` from the background threads that
//! invoke driver methods.

#![allow(clippy::result_large_err)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod connection;
pub mod driver;
pub mod edit;
pub mod error_formatter;
pub mod generator;
pub mod language_service;
pub mod paging;
pub mod schema;
pub mod session;
pub mod tls;
pub mod types;

#[cfg(test)]
mod tests;

pub use driver::{CASSANDRA_FORM, CassandraDriver, METADATA};
