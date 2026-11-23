use datatab_core::{ConnectionProfile, DbConfig, DbDriver, QueryRequest};

use crate::DuckDbDriver;

#[test]
fn bulk_columns_match_table_details() {
    let connection = DuckDbDriver::new()
        .connect(&ConnectionProfile::new("duck", DbConfig::default_duckdb()))
        .expect("connect");
    connection.execute(&QueryRequest::new(
        "CREATE SCHEMA sales; CREATE TABLE sales.orders (id INTEGER PRIMARY KEY, note VARCHAR NOT NULL DEFAULT 'x'); CREATE TABLE sales.other (value DOUBLE);"
    )).expect("create tables");
    let catalog = connection.active_database().expect("catalog");
    let bulk = connection
        .schema_columns(&catalog, Some("sales"))
        .expect("bulk columns");
    for table in ["orders", "other"] {
        let actual: Vec<_> = bulk
            .iter()
            .filter(|row| row.table_name == table)
            .map(|row| format!("{:?}", row.column))
            .collect();
        let expected: Vec<_> = connection
            .table_details(&catalog, Some("sales"), table)
            .expect("details")
            .columns
            .expect("columns")
            .iter()
            .map(|column| format!("{column:?}"))
            .collect();
        assert_eq!(actual, expected);
    }
}
