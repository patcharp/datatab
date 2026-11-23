//! Keep legacy ClickHouse rows distinguishable from explicit TLS modes without
//! changing their existing `ssl_mode = prefer` placeholder or any stored value.
use rusqlite::Transaction;

use crate::migrations::{Migration, MigrationError};

pub(crate) struct MigrationImpl;

impl Migration for MigrationImpl {
    fn name(&self) -> &str {
        "052_clickhouse_tls_mode"
    }

    fn run(&self, tx: &Transaction) -> Result<(), MigrationError> {
        let table_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'cfg_connection_driver_configs')",
            [], |row| row.get(0),
        ).map_err(sqlite_error)?;
        if !table_exists {
            return Ok(());
        }
        let column_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('cfg_connection_driver_configs') WHERE name = 'clickhouse_ssl_mode')",
            [], |row| row.get(0),
        ).map_err(sqlite_error)?;
        if column_exists {
            return Ok(());
        }
        tx.execute_batch(
            "ALTER TABLE cfg_connection_driver_configs ADD COLUMN clickhouse_ssl_mode TEXT;",
        )
        .map_err(sqlite_error)
    }
}

fn sqlite_error(source: rusqlite::Error) -> MigrationError {
    MigrationError::Sqlite {
        path: std::path::PathBuf::from("<052_clickhouse_tls_mode>"),
        source,
    }
}

#[cfg(test)]
mod tests;
