use crate::migrations::MigrationRegistry;

#[test]
fn clickhouse_tls_migration_preserves_legacy_row() {
    let connection = rusqlite::Connection::open_in_memory().expect("scratch database");
    let mut previous = MigrationRegistry::new();
    previous
        .migrations
        .retain(|migration| migration.name() != "052_clickhouse_tls_mode");
    previous.run_all(&connection).expect("previous schema");
    connection.execute_batch(
        "INSERT INTO cfg_connection_profiles (id, name, driver_id, kind, created_at, updated_at)
         VALUES ('legacy', 'legacy', 'clickhouse', 'ClickHouse', 'before', 'before');
         INSERT INTO cfg_connection_driver_configs (id, profile_id, config_key, uri)
         VALUES ('config', 'legacy', 'ClickHouse', 'https://localhost:8443');"
    ).expect("previous row");
    MigrationRegistry::new()
        .run_all(&connection)
        .expect("upgrade");
    let stored: (String, Option<String>, String) = connection.query_row(
        "SELECT ssl_mode, clickhouse_ssl_mode, uri FROM cfg_connection_driver_configs WHERE id = 'config'",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).expect("legacy row remains readable");
    assert_eq!(
        stored,
        ("prefer".into(), None, "https://localhost:8443".into())
    );
}
