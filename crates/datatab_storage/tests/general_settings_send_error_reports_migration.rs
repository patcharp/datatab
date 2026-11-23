use datatab_storage::migrations::MigrationRegistry;
use rusqlite::{Connection, Result as SqlResult};

fn read_send_error_reports(connection: &Connection) -> SqlResult<i64> {
    connection.query_row(
        "SELECT send_error_reports FROM cfg_general_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )
}

#[test]
#[expect(
    clippy::unwrap_in_result,
    reason = "fixture setup uses expect() so a failure names the setup step that broke, not just a propagated error"
)]
fn send_error_reports_migration_adds_the_default_and_persists_a_change() -> SqlResult<()> {
    let connection = Connection::open_in_memory()?;
    MigrationRegistry::new()
        .run_all(&connection)
        .expect("migration chain must apply");

    assert_eq!(
        read_send_error_reports(&connection)?,
        1,
        "an install that never saw the switch keeps offering the report"
    );

    connection.execute(
        "UPDATE cfg_general_settings SET send_error_reports = 0 WHERE id = 1",
        [],
    )?;
    assert_eq!(read_send_error_reports(&connection)?, 0);

    let null_value = connection.execute(
        "UPDATE cfg_general_settings SET send_error_reports = NULL WHERE id = 1",
        [],
    );
    assert!(null_value.is_err(), "send_error_reports must be NOT NULL");

    Ok(())
}

#[test]
#[expect(
    clippy::unwrap_in_result,
    reason = "fixture setup uses expect() so a failure names the setup step that broke, not just a propagated error"
)]
fn send_error_reports_migration_upgrades_a_populated_settings_row() -> SqlResult<()> {
    let connection = Connection::open_in_memory()?;
    let registry = MigrationRegistry::new();
    registry
        .run_all(&connection)
        .expect("migration chain must establish the fixture");

    connection
        .execute_batch("ALTER TABLE cfg_general_settings DROP COLUMN send_error_reports;")
        .expect("SQLite must support dropping the added column for the pre-051 fixture");
    connection.execute(
        "DELETE FROM sys_migrations WHERE name = '051_general_settings_send_error_reports'",
        [],
    )?;
    connection.execute(
        "UPDATE cfg_general_settings SET theme = 'light', ui_font_size = 15.0 WHERE id = 1",
        [],
    )?;

    registry
        .run_all(&connection)
        .expect("migration 051 must upgrade populated settings");

    let (theme, ui_font_size): (String, f64) = connection.query_row(
        "SELECT theme, ui_font_size FROM cfg_general_settings WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(theme, "light");
    assert_eq!(ui_font_size, 15.0);
    assert_eq!(read_send_error_reports(&connection)?, 1);

    registry
        .run_all(&connection)
        .expect("rerunning the registry must be idempotent");
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sys_migrations WHERE name = '051_general_settings_send_error_reports'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "051 must remain recorded once");

    Ok(())
}
