//! Row edits through lightweight transactions.
//!
//! CQL has no transaction to hold a read and a write together, and a plain
//! `INSERT` or `UPDATE` is an upsert: it would create the row a user meant to
//! change or re-create the one they meant to delete. Every edit therefore
//! carries a lightweight transaction — `IF EXISTS` for an update or a delete,
//! `IF NOT EXISTS` for an insert — and the server's `[applied]` answer decides
//! whether the edit happened. A refused one is an error the user sees, never a
//! silent success.
//!
//! A row is addressed by its whole primary key. A `DELETE` whose `WHERE` stops
//! at the partition key removes a whole partition, so the statement builders
//! refuse an identity that does not name exactly the table's key.

use datatab_core::{
    ColumnAssignment, DbError, DefaultSqlDialect, QueryResult, RecordIdentity, SqlDialect, Value,
};

/// What a relation is, as far as a row edit is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelationKind {
    /// A table that accepts row edits.
    Table,
    /// A materialized view, maintained by its base table.
    MaterializedView,
    /// A counter table: a counter changes by increment, never by assignment.
    CounterTable,
}

impl RelationKind {
    /// Why this relation refuses a row edit, or `None` when it accepts one.
    pub(crate) fn edit_refusal(self) -> Option<DbError> {
        match self {
            Self::Table => None,
            Self::MaterializedView => Some(DbError::NotSupported(
                "A Cassandra materialized view is read-only; edit its base table instead"
                    .to_string(),
            )),
            Self::CounterTable => Some(DbError::NotSupported(
                "A Cassandra counter table is read-only here: a counter changes by increment, not by a row edit"
                    .to_string(),
            )),
        }
    }
}

/// One prepared statement and the values bound to it, in placeholder order.
#[derive(Debug, PartialEq)]
pub(crate) struct EditStatement {
    pub(crate) cql: String,
    pub(crate) values: Vec<Value>,
}

/// `UPDATE … SET … WHERE <full key> IF EXISTS`.
pub(crate) fn update_statement(
    keyspace: &str,
    table: &str,
    key_columns: &[String],
    identity: &[(String, Value)],
    assignments: &[ColumnAssignment],
) -> Result<EditStatement, DbError> {
    if assignments.is_empty() {
        return Err(DbError::NotSupported(
            "A row update needs at least one column to change".to_string(),
        ));
    }
    let dialect = DefaultSqlDialect;
    let key = ordered_identity(key_columns, identity)?;

    let set: Vec<String> = assignments
        .iter()
        .map(|assignment| format!("{} = ?", dialect.quote_identifier(&assignment.name)))
        .collect();
    let where_clause = key_where(key_columns, &dialect);

    let mut values: Vec<Value> = assignments
        .iter()
        .map(|assignment| assignment.value.clone())
        .collect();
    values.extend(key);

    Ok(EditStatement {
        cql: format!(
            "UPDATE {} SET {} WHERE {} IF EXISTS",
            dialect.qualified_table(Some(keyspace), table),
            set.join(", "),
            where_clause
        ),
        values,
    })
}

/// `INSERT … VALUES … IF NOT EXISTS`.
pub(crate) fn insert_statement(
    keyspace: &str,
    table: &str,
    key_columns: &[String],
    assignments: &[ColumnAssignment],
) -> Result<EditStatement, DbError> {
    if assignments.is_empty() {
        return Err(DbError::NotSupported(
            "A row insert needs at least one column".to_string(),
        ));
    }
    // CQL refuses an insert that leaves a key column out, and a missing key
    // part is a mistake worth naming before the server does.
    for key_column in key_columns {
        if !assignments
            .iter()
            .any(|assignment| assignment.name == *key_column)
        {
            return Err(DbError::query_failed(
                "The insert does not name every key column of the table",
            ));
        }
    }

    let dialect = DefaultSqlDialect;
    let columns: Vec<String> = assignments
        .iter()
        .map(|assignment| dialect.quote_identifier(&assignment.name))
        .collect();
    let placeholders = vec!["?"; assignments.len()].join(", ");

    Ok(EditStatement {
        cql: format!(
            "INSERT INTO {} ({}) VALUES ({}) IF NOT EXISTS",
            dialect.qualified_table(Some(keyspace), table),
            columns.join(", "),
            placeholders
        ),
        values: assignments
            .iter()
            .map(|assignment| assignment.value.clone())
            .collect(),
    })
}

/// `DELETE … WHERE <full key> IF EXISTS`.
pub(crate) fn delete_statement(
    keyspace: &str,
    table: &str,
    key_columns: &[String],
    identity: &[(String, Value)],
) -> Result<EditStatement, DbError> {
    let dialect = DefaultSqlDialect;
    let key = ordered_identity(key_columns, identity)?;

    Ok(EditStatement {
        cql: format!(
            "DELETE FROM {} WHERE {} IF EXISTS",
            dialect.qualified_table(Some(keyspace), table),
            key_where(key_columns, &dialect)
        ),
        values: key,
    })
}

/// The identity's values in the table's key order, after checking that the
/// identity names exactly that key.
fn ordered_identity(
    key_columns: &[String],
    identity: &[(String, Value)],
) -> Result<Vec<Value>, DbError> {
    if key_columns.is_empty() {
        return Err(DbError::NotSupported(
            "Cassandra cannot address a row of a table whose key this session has not read"
                .to_string(),
        ));
    }
    if identity.len() != key_columns.len() {
        return Err(DbError::NotSupported(
            "The row's identity names a column outside the table's key, which a Cassandra edit cannot match"
                .to_string(),
        ));
    }
    let mut values = Vec::with_capacity(key_columns.len());
    for key_column in key_columns {
        let Some((_, value)) = identity.iter().find(|(column, _)| column == key_column) else {
            return Err(DbError::NotSupported(
                "The row's key is incomplete: a Cassandra edit addresses every key column, and a partial key would change or remove more than one row"
                    .to_string(),
            ));
        };
        values.push(value.clone());
    }
    Ok(values)
}

fn key_where(key_columns: &[String], dialect: &dyn SqlDialect) -> String {
    key_columns
        .iter()
        .map(|column| format!("{} = ?", dialect.quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// The identity of a row as `(column, value)` pairs, or `None` when the
/// identity cannot address one row.
///
/// The one place both the edit path and the generator read an identity, so the
/// rule they apply cannot drift. `RecordIdentity::is_valid` is the check: a
/// composite identity needs a column for each value, and at least one of each.
pub(crate) fn composite_identity(identity: &RecordIdentity) -> Option<Vec<(String, Value)>> {
    if !identity.is_valid() {
        return None;
    }
    match identity {
        RecordIdentity::Composite { columns, values } => Some(
            columns
                .iter()
                .cloned()
                .zip(values.iter().cloned())
                .collect(),
        ),
        _ => None,
    }
}

/// Why an identity cannot address one row.
pub(crate) fn identity_refusal() -> DbError {
    DbError::NotSupported(
        "A Cassandra row is addressed by its key columns and their values".to_string(),
    )
}

/// Why a lightweight transaction refused the edit, or `None` when it applied.
///
/// A CQL lightweight transaction answers with a row whose first column is
/// `[applied]`. `false` means the row the statement matched was not in the
/// state the guard required — it was deleted, or already there — and nothing
/// was written.
pub(crate) fn not_applied(result: &QueryResult, what: &str) -> Option<DbError> {
    let applied_ix = result
        .columns
        .iter()
        .position(|column| column.name == APPLIED_COLUMN)?;
    match result.rows.first().and_then(|row| row.get(applied_ix)) {
        Some(Value::Bool(true)) => None,
        Some(Value::Bool(false)) => Some(DbError::query_failed(format!(
            "{what} was not applied: the row is not in the state the edit required"
        ))),
        _ => Some(DbError::query_failed(format!(
            "{what} did not report whether the server applied it"
        ))),
    }
}

/// The column a lightweight transaction reports its outcome in.
const APPLIED_COLUMN: &str = "[applied]";
