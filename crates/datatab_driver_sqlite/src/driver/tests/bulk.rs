use std::sync::{Arc, Mutex};

use datatab_core::{Connection, IndexData};
use rusqlite::Connection as RawConnection;

use super::super::{SqliteConnection, SqliteConnectionState};

#[test]
fn bulk_metadata_matches_table_details() {
    let raw = RawConnection::open_in_memory().expect("open SQLite");
    raw.execute_batch(
        "CREATE TABLE parent (id TEXT PRIMARY KEY);
         CREATE TABLE child (id TEXT PRIMARY KEY, parent_id TEXT REFERENCES parent(id) ON DELETE CASCADE ON UPDATE SET NULL, \
             name TEXT NOT NULL DEFAULT 'test');
         CREATE INDEX child_name ON child(name);
         CREATE INDEX child_expression ON child(lower(name));",
    )
    .expect("create schema");
    let connection =
        SqliteConnection::for_test(Arc::new(Mutex::new(SqliteConnectionState::new(raw))));
    let details = connection
        .table_details("main", None, "child")
        .expect("details");
    let bulk_columns = connection
        .schema_columns("main", None)
        .expect("bulk columns");
    let columns: Vec<_> = bulk_columns
        .into_iter()
        .filter(|row| row.table_name == "child")
        .map(|row| format!("{:?}", row.column))
        .collect();
    let expected: Vec<_> = details
        .columns
        .expect("columns")
        .iter()
        .map(|column| format!("{column:?}"))
        .collect();
    assert_eq!(columns, expected);

    let mut bulk_indexes: Vec<_> = connection
        .schema_indexes("main", None)
        .expect("bulk indexes")
        .into_iter()
        .filter(|index| index.table_name == "child")
        .map(|index| (index.name, index.columns, index.is_unique, index.is_primary))
        .collect();
    let IndexData::Relational(indexes) = details.indexes.expect("indexes") else {
        panic!("relational indexes")
    };
    let mut expected_indexes: Vec<_> = indexes
        .into_iter()
        .map(|index| (index.name, index.columns, index.is_unique, index.is_primary))
        .collect();
    bulk_indexes.sort();
    expected_indexes.sort();
    assert_eq!(bulk_indexes, expected_indexes);

    let bulk_keys = connection
        .schema_foreign_keys("main", None)
        .expect("bulk foreign keys");
    let key = bulk_keys
        .iter()
        .find(|key| key.table_name == "child")
        .expect("child key");
    let expected_key = &details.foreign_keys.expect("foreign keys")[0];
    assert_eq!(
        (
            &key.name,
            &key.columns,
            &key.referenced_schema,
            &key.referenced_table,
            &key.referenced_columns,
            &key.on_delete,
            &key.on_update
        ),
        (
            &expected_key.name,
            &expected_key.columns,
            &expected_key.referenced_schema,
            &expected_key.referenced_table,
            &expected_key.referenced_columns,
            &expected_key.on_delete,
            &expected_key.on_update
        )
    );
}
