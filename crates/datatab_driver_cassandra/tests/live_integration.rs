#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::result_large_err
)]

//! The live suites for the Cassandra and ScyllaDB driver (#64 part 5).
//!
//! Every test is `#[ignore = "requires Docker daemon"]`. Each one starts a
//! single throwaway container, waits until it answers CQL, creates the fixture
//! keyspace, and removes the container and the keyspace at the end. One
//! container runs at a time: the helpers hold a lock for a container's whole
//! life, because both images publish CQL port 9042 on the host 1:1 as the
//! driver's request pool requires, and one server alone can take a gigabyte and
//! a half. Both images are pulled in advance — the helpers pass `--pull=never`.
//!
//! ```text
//! # Cassandra 5.0, and the one test part 1 carried
//! DATATAB_TEST_SCRATCH=<folder> cargo test -p datatab_driver_cassandra \
//!   --test live_integration --locked -- cassandra_live_connect_reads_the_release_version \
//!   --ignored --test-threads=1
//!
//! # Cassandra 5.0: the whole suite on a password-protected port
//! DATATAB_TEST_SCRATCH=<folder> cargo test -p datatab_driver_cassandra \
//!   --test live_integration --locked -- cassandra_live_suite --ignored --test-threads=1
//!
//! # ScyllaDB 6.2: the same suite
//! DATATAB_TEST_SCRATCH=<folder> cargo test -p datatab_driver_cassandra \
//!   --test live_integration --locked -- scylla_live_suite --ignored --test-threads=1
//!
//! # ScyllaDB 6.2 over TLS: also needs the `openssl` command
//! DATATAB_TEST_SCRATCH=<folder> cargo test -p datatab_driver_cassandra \
//!   --test live_integration --locked -- scylla_live_tls_modes --ignored --test-threads=1
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
use datatab_core::secrecy::SecretString;
use datatab_core::{
    ColumnKind, Connection, ConnectionProfile, CrudResult, CustomTypeKind, DangerousQueryKind,
    DbConfig, DbDriver, DbError, DbKind, ExecutionClassification, IndexData, Pagination,
    QueryRequest, QueryResult, RowDelete, RowIdentity, RowInsert, RowPatch, SchemaFeatures,
    SchemaLoadingStrategy, SchemaSnapshotAuthority, TableBrowseRequest, TableCountRequest,
    TableRef, Value,
};
use datatab_driver_cassandra::CassandraDriver;
use datatab_test_support::containers;

/// The keyspace every test here creates and drops.
const KEYSPACE: &str = "datatab_live";

/// The partition key of the fixture's `events` rows, and of the counter row.
const EVENTS_BUCKET: i32 = 7;

/// How many rows the paging table holds, and how many a page shows: three
/// pages, the last one short.
const PAGE_ROWS: i32 = 25;
const PAGE_LIMIT: u32 = 10;

/// Which server a suite runs against.
///
/// The one difference the fixture needs: ScyllaDB 6.x creates a keyspace with
/// tablets by default, and a materialized view is not supported on a tablet
/// keyspace, so the ScyllaDB keyspace turns tablets off. Cassandra has no such
/// option and needs none.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Server {
    Cassandra,
    Scylla,
}

impl Server {
    fn keyspace_options(self) -> &'static str {
        match self {
            Self::Cassandra => "",
            Self::Scylla => " AND tablets = {'enabled': false}",
        }
    }
}

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

/// A profile for a suite container, which always requires the image's own role.
fn profile(
    endpoint: &containers::CassandraEndpoint,
    ssl_mode: &str,
    ssl_root_cert_path: Option<&Path>,
) -> ConnectionProfile {
    ConnectionProfile::new(
        "live-cassandra",
        DbConfig::Cassandra {
            contact_points: endpoint.host.clone(),
            port: endpoint.port,
            datacenter: String::new(),
            keyspace: String::new(),
            user: containers::CQL_TEST_USER.to_string(),
            ssl_mode: Some(ssl_mode.to_string()),
            ssl_root_cert_path: ssl_root_cert_path.map(|path| path.display().to_string()),
            ssl_client_cert_path: None,
            ssl_client_key_path: None,
            ssh_tunnel: None,
            ssh_tunnel_profile_id: None,
        },
    )
}

fn connect_attempt(
    profile: &ConnectionProfile,
    password: Option<&str>,
) -> Result<Box<dyn Connection>, DbError> {
    let driver = CassandraDriver::new();
    let password = password.map(|password| SecretString::from(password.to_string()));
    driver.connect_with_secrets(profile, password.as_ref(), None)
}

/// A connection that answers a ping. A fresh container needs a moment after it
/// logs that CQL is listening, so the first attempts are retried.
fn connect(profile: &ConnectionProfile) -> Result<Box<dyn Connection>, DbError> {
    containers::retry_db_operation(Duration::from_secs(120), || {
        let connection = connect_attempt(profile, Some(containers::CQL_TEST_PASSWORD))?;
        connection.ping()?;
        Ok(connection)
    })
}

/// The password path, and the two ways it must refuse: no password at all, and
/// the wrong one. Both are checked on a container that has already answered, so
/// a slow start cannot be mistaken for a refusal.
fn assert_password_is_required(
    endpoint: &containers::CassandraEndpoint,
    connection: &dyn Connection,
) -> Result<(), DbError> {
    assert_eq!(connection.kind(), DbKind::Cassandra);

    let anonymous = connect_attempt(&profile(endpoint, "disable", None), None).err();
    assert!(
        matches!(anonymous, Some(DbError::AuthFailed(_))),
        "a password-protected port refuses a connection without one: {anonymous:?}"
    );

    let wrong = connect_attempt(
        &profile(endpoint, "disable", None),
        Some("not-the-password"),
    )
    .err();
    assert!(
        matches!(wrong, Some(DbError::AuthFailed(_))),
        "a password-protected port refuses the wrong password: {wrong:?}"
    );
    Ok(())
}

/// The release the container reports, which also says which server it is.
fn assert_release(connection: &dyn Connection, expected_prefix: &str) -> Result<(), DbError> {
    let result = connection.execute(&QueryRequest::new(
        "SELECT release_version FROM system.local",
    ))?;
    assert_eq!(result.columns.len(), 1);
    assert_eq!(result.columns[0].name, "release_version");
    assert_eq!(result.columns[0].type_name, "text");
    assert_eq!(result.rows.len(), 1);
    let Value::Text(version) = &result.rows[0][0] else {
        panic!("release_version is text, got {:?}", result.rows[0][0]);
    };
    assert!(
        version.starts_with(expected_prefix),
        "expected a {expected_prefix} release, got '{version}'"
    );
    Ok(())
}

/// The ScyllaDB build the container reports.
///
/// ScyllaDB answers `system.local.release_version` with the Cassandra version it
/// emulates (`3.0.8`), never its own, so its build comes from
/// `system.versions`, where the row keyed `local` carries it.
fn assert_scylla_release(connection: &dyn Connection) -> Result<(), DbError> {
    let result = connection.execute(&QueryRequest::new(
        "SELECT version FROM system.versions WHERE key = 'local'",
    ))?;
    assert_eq!(result.columns.len(), 1);
    assert_eq!(result.columns[0].name, "version");
    assert_eq!(result.columns[0].type_name, "text");
    assert_eq!(result.rows.len(), 1);
    let Value::Text(version) = &result.rows[0][0] else {
        panic!("version is text, got {:?}", result.rows[0][0]);
    };
    assert!(
        version.starts_with("6.2"),
        "expected a ScyllaDB 6.2 build, got '{version}'"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

fn execute(connection: &dyn Connection, cql: &str) -> Result<(), DbError> {
    connection.execute(&QueryRequest::new(cql.to_string()))?;
    Ok(())
}

/// The keyspace, a user type, a table with every column role, its index, a
/// materialized view over it, a counter table, a table with one column per CQL
/// type, and the table the paging test walks.
fn create_fixture(connection: &dyn Connection, server: Server) -> Result<(), DbError> {
    for statement in [
        format!(
            "CREATE KEYSPACE IF NOT EXISTS {KEYSPACE} WITH replication = \
             {{'class': 'SimpleStrategy', 'replication_factor': 1}}{}",
            server.keyspace_options()
        ),
        format!("CREATE TYPE IF NOT EXISTS {KEYSPACE}.address (street text, zip int)"),
        format!(
            "CREATE TABLE IF NOT EXISTS {KEYSPACE}.events (\
             bucket int, at timestamp, who text, note text static, payload text, \
             PRIMARY KEY ((bucket), who, at)) \
             WITH CLUSTERING ORDER BY (who ASC, at DESC)"
        ),
        format!("CREATE INDEX IF NOT EXISTS events_payload_idx ON {KEYSPACE}.events (payload)"),
        format!(
            "CREATE MATERIALIZED VIEW IF NOT EXISTS {KEYSPACE}.events_by_who AS \
             SELECT bucket, at, who, payload FROM {KEYSPACE}.events \
             WHERE bucket IS NOT NULL AND who IS NOT NULL AND at IS NOT NULL \
             PRIMARY KEY (who, bucket, at)"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {KEYSPACE}.hits (\
             bucket int, at timestamp, total counter, PRIMARY KEY ((bucket), at))"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {KEYSPACE}.every_type (\
             id int PRIMARY KEY, \
             c_ascii ascii, c_bigint bigint, c_blob blob, c_boolean boolean, \
             c_date date, c_decimal decimal, c_duration duration, c_double double, \
             c_float float, c_int int, c_inet inet, c_smallint smallint, c_text text, \
             c_timestamp timestamp, c_time time, c_timeuuid timeuuid, c_tinyint tinyint, \
             c_uuid uuid, c_varint varint, c_list list<int>, c_set set<text>, \
             c_map map<text, int>, c_tuple tuple<int, text>, \
             c_address frozen<{KEYSPACE}.address>)"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {KEYSPACE}.pages (\
             bucket int, seq int, label text, PRIMARY KEY ((bucket), seq))"
        ),
    ] {
        execute(connection, &statement)?;
    }

    // The paging rows are clustering rows of one partition, so their order is
    // the server's own and three pages can be checked row by row.
    for seq in 1..=PAGE_ROWS {
        execute(
            connection,
            &format!(
                "INSERT INTO {KEYSPACE}.pages (bucket, seq, label) VALUES (1, {seq}, 'row-{seq}')"
            ),
        )?;
    }
    Ok(())
}

/// Drops the keyspace, and with it everything the suite created. Dropping it
/// through the connection also proves the fixture's DDL really ran.
fn drop_fixture(connection: &dyn Connection) -> Result<(), DbError> {
    execute(connection, &format!("DROP KEYSPACE IF EXISTS {KEYSPACE}"))
}

/// Everything design §7 proves on both servers, in the one order that works:
/// the schema read fills the classifier's key cache, so classification follows
/// it, and the fixture is dropped last.
fn run_suite(connection: &dyn Connection) -> Result<(), DbError> {
    schema_browse(connection)?;
    classification(connection)?;
    every_type_round_trips(connection)?;
    three_pages_forward_and_back(connection)?;
    edits_by_key(connection)?;
    read_only_relations(connection)?;
    formatted_server_error(connection)?;
    drop_fixture(connection)
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

fn schema_browse(connection: &dyn Connection) -> Result<(), DbError> {
    // The shape the tree relies on: keyspaces first, then the objects of the
    // one a user opens.
    assert_eq!(
        connection.schema_loading_strategy(),
        SchemaLoadingStrategy::LazyPerDatabase
    );
    assert!(
        connection
            .schema_features()
            .contains(SchemaFeatures::CUSTOM_TYPES)
    );
    // The snapshot lists keyspaces only, so the tree must not read it as
    // "this keyspace has no objects".
    assert_eq!(
        connection.schema_snapshot_authority(),
        SchemaSnapshotAuthority::EnumerationOnly
    );
    assert_keyspaces(connection)?;
    assert_keyspace_objects(connection)?;
    assert_table_details(connection)?;
    assert_missing_object(connection)
}

/// The keyspaces of the cluster, the server's own last.
fn assert_keyspaces(connection: &dyn Connection) -> Result<(), DbError> {
    let databases = connection.list_databases()?;
    let names: Vec<&str> = databases.iter().map(|entry| entry.name.as_str()).collect();
    assert!(names.contains(&KEYSPACE), "keyspaces: {names:?}");
    assert!(names.contains(&"system_schema"), "keyspaces: {names:?}");
    let fixture_at = names.iter().position(|name| *name == KEYSPACE).unwrap();
    let system_at = names.iter().position(|name| *name == "system").unwrap();
    assert!(
        fixture_at < system_at,
        "a user keyspace is listed before the server's own: {names:?}"
    );

    Ok(())
}

/// The objects of one keyspace, names only.
fn assert_keyspace_objects(connection: &dyn Connection) -> Result<(), DbError> {
    let schema = connection.schema_for_database(KEYSPACE)?;
    let tables: Vec<&str> = schema
        .tables
        .iter()
        .map(|table| table.name.as_str())
        .collect();
    for expected in ["events", "hits", "every_type", "pages"] {
        assert!(tables.contains(&expected), "tables: {tables:?}");
    }
    let views: Vec<&str> = schema.views.iter().map(|view| view.name.as_str()).collect();
    assert!(views.contains(&"events_by_who"), "views: {views:?}");
    // A materialized view is a view, not also a table: listing it in both
    // places would put the same object in the tree twice.
    assert!(
        !tables.contains(&"events_by_who"),
        "a view is listed as a table: {tables:?}"
    );
    // A table listed shallowly has not read its columns yet.
    assert!(schema.tables.iter().all(|table| table.columns.is_none()));
    let types = schema.custom_types.unwrap_or_default();
    assert_eq!(types.len(), 1);
    assert_eq!(types[0].name, "address");
    assert_eq!(types[0].kind, CustomTypeKind::Composite);

    Ok(())
}

/// One table's columns, key roles, hints and index; the view's base table; the
/// counter table's column; and the bulk paths that answer the same.
fn assert_table_details(connection: &dyn Connection) -> Result<(), DbError> {
    let events = connection.table_details(KEYSPACE, None, "events")?;
    let columns = events.columns.unwrap_or_default();
    let ordered: Vec<&str> = columns.iter().map(|column| column.name.as_str()).collect();
    assert_eq!(ordered, vec!["bucket", "who", "at", "note", "payload"]);
    let keys: Vec<&str> = columns
        .iter()
        .filter(|column| column.is_primary_key)
        .map(|column| column.name.as_str())
        .collect();
    assert_eq!(keys, vec!["bucket", "who", "at"]);

    let hints = events.storage_hints.unwrap_or_default();
    let hint = |label: &str| {
        hints
            .iter()
            .find(|hint| hint.label == label)
            .unwrap_or_else(|| panic!("a {label} hint is present: {hints:?}"))
    };
    assert_eq!(hint("Partition key").columns, vec!["bucket"]);
    assert_eq!(hint("Clustering key").columns, vec!["who", "at"]);
    assert_eq!(hint("Clustering order").columns, vec!["at"]);
    assert_eq!(hint("Static column").columns, vec!["note"]);

    let Some(IndexData::Relational(indexes)) = events.indexes else {
        panic!("events carries relational index metadata");
    };
    let payload_index = indexes
        .iter()
        .find(|index| index.name == "events_payload_idx")
        .unwrap_or_else(|| panic!("events_payload_idx is listed: {indexes:?}"));
    assert_eq!(payload_index.columns, vec!["payload"]);

    // A materialized view points at the table that maintains it.
    let view = connection.table_details(KEYSPACE, None, "events_by_who")?;
    let view_hints = view.storage_hints.unwrap_or_default();
    let base = view_hints
        .iter()
        .find(|hint| hint.label == "Base table")
        .unwrap_or_else(|| panic!("the view names its base table: {view_hints:?}"));
    assert_eq!(base.detail.as_deref(), Some("events"));

    // A counter table reads like any other, so the tree can list it; only the
    // edit path treats it differently.
    let counter = connection.table_details(KEYSPACE, None, "hits")?;
    let counter_columns = counter.columns.unwrap_or_default();
    let total = counter_columns
        .iter()
        .find(|column| column.name == "total")
        .unwrap_or_else(|| panic!("hits carries total: {counter_columns:?}"));
    assert_eq!(total.type_name, "counter");

    // The bulk column path agrees with the per-table one.
    let bulk = connection.schema_columns(KEYSPACE, None)?;
    let event_columns = bulk
        .iter()
        .filter(|entry| entry.table_name == "events")
        .count();
    assert_eq!(event_columns, 5);

    // The bulk index path (one statement per keyspace) finds the fixture's
    // index too, and reads the same columns as the per-table list. The fixture
    // keyspace holds exactly this one index, so a duplicate row would fail here.
    let bulk_indexes = connection.schema_indexes(KEYSPACE, None)?;
    assert_eq!(bulk_indexes.len(), 1, "indexes: {bulk_indexes:?}");
    let bulk_payload = bulk_indexes
        .iter()
        .find(|index| index.name == "events_payload_idx")
        .unwrap_or_else(|| panic!("the bulk path lists the index: {bulk_indexes:?}"));
    assert_eq!(bulk_payload.table_name, "events");
    assert_eq!(bulk_payload.columns, vec!["payload"]);

    Ok(())
}

/// An object that is not there is reported as missing, not as empty.
fn assert_missing_object(connection: &dyn Connection) -> Result<(), DbError> {
    let missing = connection.table_details(KEYSPACE, None, "not_a_table");
    assert!(
        matches!(missing, Err(DbError::ObjectNotFound(_))),
        "a missing table is ObjectNotFound, got {missing:?}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Classification, through the connection's own service
// ---------------------------------------------------------------------------

/// The classifier judges a `DELETE` by the key the schema read returned, so a
/// live read is what makes the danger table mean anything here.
fn classification(connection: &dyn Connection) -> Result<(), DbError> {
    let service = connection.language_service();

    assert_eq!(
        service.classify_execution(&format!("SELECT * FROM {KEYSPACE}.events")),
        Some(ExecutionClassification::Read)
    );
    assert_eq!(
        service.classify_execution("SELECT release_version FROM system.local"),
        Some(ExecutionClassification::Metadata),
        "the server's own keyspace is metadata, not user data"
    );

    let keyed =
        format!("DELETE FROM {KEYSPACE}.events WHERE bucket = 7 AND who = 'alice' AND at = 1");
    assert_eq!(
        service.classify_execution(&keyed),
        Some(ExecutionClassification::Write),
        "the key read from the live schema makes this one row"
    );
    assert_eq!(service.detect_dangerous(&keyed), None);

    let partial = format!("DELETE FROM {KEYSPACE}.events WHERE bucket = 7");
    assert_eq!(
        service.classify_execution(&partial),
        Some(ExecutionClassification::Destructive),
        "the partition key alone leaves every row of the partition in range"
    );
    assert_eq!(
        service.detect_dangerous(&partial),
        Some(DangerousQueryKind::CqlPartialKeyDelete)
    );

    let drop_table = format!("DROP TABLE {KEYSPACE}.events");
    assert_eq!(
        service.classify_execution(&drop_table),
        Some(ExecutionClassification::AdminDestructive)
    );
    assert_eq!(
        service.detect_dangerous(&drop_table),
        Some(DangerousQueryKind::Drop)
    );
    assert_eq!(
        service.detect_dangerous(&format!("TRUNCATE {KEYSPACE}.events")),
        Some(DangerousQueryKind::Truncate)
    );

    // The bulk schema read fills the keys of every table too, so a single-key
    // table is judged by what `system_schema` said about it.
    assert_eq!(
        service.classify_execution(&format!("DELETE FROM {KEYSPACE}.every_type WHERE id = 1")),
        Some(ExecutionClassification::Write)
    );
    // A table no read has seen is never called keyed, however complete the
    // `WHERE` looks.
    assert_eq!(
        service.detect_dangerous(&format!("DELETE FROM {KEYSPACE}.never_read WHERE id = 1")),
        Some(DangerousQueryKind::CqlPartialKeyDelete)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One row with a value of every type. The timestamp is bound rather than
/// written as a literal, because how a server reads a text date is its own
/// business; every other literal is the CQL form.
fn insert_every_type(connection: &dyn Connection) -> Result<(), DbError> {
    let mut request = QueryRequest::new(format!(
        "INSERT INTO {KEYSPACE}.every_type (\
         id, c_ascii, c_bigint, c_blob, c_boolean, c_date, c_decimal, c_duration, \
         c_double, c_float, c_int, c_inet, c_smallint, c_text, c_timestamp, c_time, \
         c_timeuuid, c_tinyint, c_uuid, c_varint, c_list, c_set, c_map, c_tuple, c_address) \
         VALUES (1, 'plain', 9007199254740993, 0xdeadbeef, true, '2026-01-02', 12.34, \
         1h30m, 1.5, 2.5, 42, '10.0.0.7', 7, 'hello', \
         ?, '03:04:05.000000006', now(), 3, \
         11111111-2222-3333-4444-555555555555, 123456789012345678901234567890, \
         [3, 1, 2], {{'b', 'a'}}, {{'k': 1, 'j': 2}}, (7, 'seven'), \
         {{street: 'main', zip: 12345}})"
    ));
    request.params = vec![Value::DateTime(every_type_time())];
    connection.execute(&request)?;
    Ok(())
}

/// The timestamp the type table holds.
fn every_type_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap() + chrono::Duration::milliseconds(678)
}

fn cell<'a>(result: &'a QueryResult, name: &str) -> &'a Value {
    let column = result
        .columns
        .iter()
        .position(|column| column.name == name)
        .unwrap_or_else(|| panic!("the result carries {name}"));
    &result.rows[0][column]
}

/// One row through every CQL native type, collection, tuple and user type, read
/// back as the grid value and the chart kind each one maps to.
fn every_type_round_trips(connection: &dyn Connection) -> Result<(), DbError> {
    insert_every_type(connection)?;

    let result = connection.execute(&QueryRequest::new(format!(
        "SELECT * FROM {KEYSPACE}.every_type WHERE id = 1"
    )))?;
    assert_eq!(result.rows.len(), 1);
    assert_every_type_column(&result);
    assert_every_type_value(&result);
    Ok(())
}

/// Every column's server type and chart kind, so a type that arrives as the
/// wrong kind is caught here rather than in a chart.
fn assert_every_type_column(result: &QueryResult) {
    let expected: [(&str, &str, ColumnKind); 25] = [
        ("id", "int", ColumnKind::Integer),
        ("c_ascii", "ascii", ColumnKind::Text),
        ("c_bigint", "bigint", ColumnKind::Integer),
        ("c_blob", "blob", ColumnKind::Unknown),
        ("c_boolean", "boolean", ColumnKind::Unknown),
        ("c_date", "date", ColumnKind::Timestamp),
        ("c_decimal", "decimal", ColumnKind::Float),
        ("c_duration", "duration", ColumnKind::Text),
        ("c_double", "double", ColumnKind::Float),
        ("c_float", "float", ColumnKind::Float),
        ("c_int", "int", ColumnKind::Integer),
        ("c_inet", "inet", ColumnKind::Text),
        ("c_smallint", "smallint", ColumnKind::Integer),
        ("c_text", "text", ColumnKind::Text),
        ("c_timestamp", "timestamp", ColumnKind::Timestamp),
        ("c_time", "time", ColumnKind::Text),
        ("c_timeuuid", "timeuuid", ColumnKind::Text),
        ("c_tinyint", "tinyint", ColumnKind::Integer),
        ("c_uuid", "uuid", ColumnKind::Text),
        ("c_varint", "varint", ColumnKind::Integer),
        ("c_list", "list<int>", ColumnKind::Unknown),
        ("c_set", "set<text>", ColumnKind::Unknown),
        ("c_map", "map<text, int>", ColumnKind::Unknown),
        ("c_tuple", "tuple<int, text>", ColumnKind::Unknown),
        ("c_address", "address", ColumnKind::Unknown),
    ];
    assert_eq!(result.columns.len(), expected.len());
    for (name, type_name, kind) in expected {
        let column = result
            .columns
            .iter()
            .find(|column| column.name == name)
            .unwrap_or_else(|| panic!("the result carries {name}: {:?}", result.columns));
        assert_eq!(column.type_name, type_name, "{name}");
        assert_eq!(column.kind, kind, "{name}");
    }
}

/// What each type reads as in the grid.
fn assert_every_type_value(result: &QueryResult) {
    assert_eq!(cell(result, "id"), &Value::Int(1));
    assert_eq!(cell(result, "c_ascii"), &Value::Text("plain".to_string()));
    assert_eq!(cell(result, "c_bigint"), &Value::Int(9_007_199_254_740_993));
    assert_eq!(
        cell(result, "c_blob"),
        &Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])
    );
    assert_eq!(cell(result, "c_boolean"), &Value::Bool(true));
    assert_eq!(
        cell(result, "c_date"),
        &Value::Date(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap())
    );
    assert_eq!(
        cell(result, "c_decimal"),
        &Value::Decimal("12.34".to_string()),
        "a decimal keeps the scale the server sent"
    );
    assert_eq!(
        cell(result, "c_duration"),
        &Value::Text("0mo0d5400000000000ns".to_string()),
        "a duration renders as its months, days and nanoseconds"
    );
    assert_eq!(cell(result, "c_double"), &Value::Float(1.5));
    assert_eq!(cell(result, "c_float"), &Value::Float(2.5));
    assert_eq!(cell(result, "c_int"), &Value::Int(42));
    assert_eq!(cell(result, "c_inet"), &Value::Text("10.0.0.7".to_string()));
    assert_eq!(cell(result, "c_smallint"), &Value::Int(7));
    assert_eq!(cell(result, "c_text"), &Value::Text("hello".to_string()));
    assert_eq!(
        cell(result, "c_timestamp"),
        &Value::DateTime(every_type_time())
    );
    assert_eq!(
        cell(result, "c_time"),
        &Value::Time(NaiveTime::from_hms_nano_opt(3, 4, 5, 6).unwrap())
    );
    let Value::Text(timeuuid) = cell(result, "c_timeuuid") else {
        panic!(
            "a timeuuid reads as text, got {:?}",
            cell(result, "c_timeuuid")
        );
    };
    assert!(
        uuid::Uuid::parse_str(timeuuid).is_ok(),
        "a timeuuid keeps its uuid form: {timeuuid}"
    );
    assert_eq!(cell(result, "c_tinyint"), &Value::Int(3));
    assert_eq!(
        cell(result, "c_uuid"),
        &Value::Text("11111111-2222-3333-4444-555555555555".to_string())
    );
    assert_eq!(
        cell(result, "c_varint"),
        &Value::Decimal("123456789012345678901234567890".to_string()),
        "a varint past i64 stays exact text rather than being rounded"
    );
    assert_eq!(
        cell(result, "c_list"),
        &Value::Array(vec![Value::Int(3), Value::Int(1), Value::Int(2)]),
        "a list keeps its order"
    );
    assert_eq!(
        cell(result, "c_set"),
        &Value::Array(vec![
            Value::Text("a".to_string()),
            Value::Text("b".to_string())
        ])
    );
    let mut expected_map = BTreeMap::new();
    expected_map.insert("j".to_string(), Value::Int(2));
    expected_map.insert("k".to_string(), Value::Int(1));
    assert_eq!(cell(result, "c_map"), &Value::Document(expected_map));
    assert_eq!(
        cell(result, "c_tuple"),
        &Value::Array(vec![Value::Int(7), Value::Text("seven".to_string())])
    );
    let mut expected_address = BTreeMap::new();
    expected_address.insert("street".to_string(), Value::Text("main".to_string()));
    expected_address.insert("zip".to_string(), Value::Int(12345));
    assert_eq!(
        cell(result, "c_address"),
        &Value::Document(expected_address),
        "a user type reads as a document of its fields"
    );
}

// ---------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------

fn pages_request(offset: u64) -> TableBrowseRequest {
    TableBrowseRequest::new(TableRef {
        schema: Some(KEYSPACE.to_string()),
        name: "pages".to_string(),
    })
    .with_pagination(Pagination::Offset {
        limit: PAGE_LIMIT,
        offset,
    })
}

/// The `seq` column of a page, in the order the grid would show it.
fn seqs(result: &QueryResult) -> Vec<i64> {
    let at = result
        .columns
        .iter()
        .position(|column| column.name == "seq")
        .unwrap_or_else(|| panic!("a page carries seq: {:?}", result.columns));
    result
        .rows
        .iter()
        .map(|row| match row[at] {
            Value::Int(seq) => seq,
            ref other => panic!("seq is an int, got {other:?}"),
        })
        .collect()
}

/// Every `seq` of the paging partition, read as a statement so the browse's
/// paging cache is left alone.
fn sequences(connection: &dyn Connection) -> Result<Vec<i64>, DbError> {
    let result = connection.execute(&QueryRequest::new(format!(
        "SELECT seq FROM {KEYSPACE}.pages WHERE bucket = 1"
    )))?;
    Ok(result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::Int(seq) => *seq,
            other => panic!("seq is an int, got {other:?}"),
        })
        .collect())
}

fn page(connection: &dyn Connection, offset: u64) -> Result<Vec<i64>, DbError> {
    Ok(seqs(&connection.browse_table(&pages_request(offset))?))
}

/// Three pages forward, then back, over one partition's clustering rows: CQL
/// has no `OFFSET`, so every page but the first is answered from the server's
/// paging state, and a return to a page must read it again, not from the start.
fn three_pages_forward_and_back(connection: &dyn Connection) -> Result<(), DbError> {
    assert_eq!(page(connection, 0)?, (1..=10).collect::<Vec<i64>>());
    assert_eq!(page(connection, 10)?, (11..=20).collect::<Vec<i64>>());
    assert_eq!(
        page(connection, 20)?,
        (21..=25).collect::<Vec<i64>>(),
        "the last page is short, and the browse reports the rows there are"
    );
    assert_eq!(
        page(connection, 10)?,
        (11..=20).collect::<Vec<i64>>(),
        "going back reads the page the cache starts it at"
    );

    // A page read from the state recorded earlier, not from a walk that starts
    // at row 0. Rows 1 to 5 sit before the state page 2 starts at, so a walk
    // would now answer offset 10 with rows 16 to 25; only the state recorded
    // before the deletion still answers 11 to 20.
    //
    // The deletion is checked with a statement, never with a browse: browsing
    // page 1 here would re-record page 2's start from the changed rows, and the
    // read that follows would prove nothing.
    for seq in 1..=5 {
        execute(
            connection,
            &format!("DELETE FROM {KEYSPACE}.pages WHERE bucket = 1 AND seq = {seq}"),
        )?;
    }
    assert_eq!(
        sequences(connection)?,
        (6..=25).collect::<Vec<i64>>(),
        "rows 1 to 5 are gone"
    );
    assert_eq!(
        page(connection, 10)?,
        (11..=20).collect::<Vec<i64>>(),
        "page 2 comes from the state recorded before the deletion, not a re-walk"
    );
    assert_eq!(
        page(connection, 0)?,
        (6..=15).collect::<Vec<i64>>(),
        "the first page always starts at the beginning of the scan"
    );

    // A row count is a full scan, so it is refused rather than read; the grid
    // pages on a short page instead.
    let count = connection.count_table(&TableCountRequest::new(TableRef {
        schema: Some(KEYSPACE.to_string()),
        name: "pages".to_string(),
    }));
    assert!(
        matches!(count, Err(DbError::NotSupported(_))),
        "a row count is refused: {count:?}"
    );

    // An offset that is not a whole page has no paging state to start from.
    let partial = connection.browse_table(&pages_request(5));
    assert!(
        matches!(partial, Err(DbError::NotSupported(_))),
        "a partial-page offset is refused, not answered with the wrong rows: {partial:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

/// The times the edit tests use, bound as values: a timestamp literal would
/// depend on how each server parses a text date.
fn event_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

fn event_identity(who: &str) -> RowIdentity {
    RowIdentity::composite(
        vec!["bucket".to_string(), "who".to_string(), "at".to_string()],
        vec![
            Value::Int(i64::from(EVENTS_BUCKET)),
            Value::Text(who.to_string()),
            Value::DateTime(event_time()),
        ],
    )
}

fn payloads(connection: &dyn Connection, who: &str) -> Result<Vec<Value>, DbError> {
    let mut request = QueryRequest::new(format!(
        "SELECT payload FROM {KEYSPACE}.events WHERE bucket = ? AND who = ?"
    ));
    request.params = vec![
        Value::Int(i64::from(EVENTS_BUCKET)),
        Value::Text(who.to_string()),
    ];
    let result = connection.execute(&request)?;
    Ok(result.rows.into_iter().map(|row| row[0].clone()).collect())
}

/// Why an edit was refused, which the user sees: a lightweight transaction that
/// did not apply is an error, never a silent success.
fn refusal(result: Result<CrudResult, DbError>, contains: &str) {
    match result {
        Err(DbError::QueryFailed(formatted)) => {
            assert!(
                formatted.message.contains(contains),
                "the refusal says '{contains}': {}",
                formatted.message
            );
        }
        Err(other) => panic!("expected a refused edit, got {other:?}"),
        Ok(_) => panic!("expected a refused edit, got a success"),
    }
}

fn edits_by_key(connection: &dyn Connection) -> Result<(), DbError> {
    // An insert, then the same key again: `IF NOT EXISTS` refuses to replace a
    // row, so a second insert cannot quietly overwrite one.
    let insert = RowInsert::new(
        "events".to_string(),
        Some(KEYSPACE.to_string()),
        vec![
            "bucket".to_string(),
            "who".to_string(),
            "at".to_string(),
            "payload".to_string(),
        ],
        vec![
            Value::Int(i64::from(EVENTS_BUCKET)),
            Value::Text("alice".to_string()),
            Value::DateTime(event_time()),
            Value::Text("first".to_string()),
        ],
    );
    assert_eq!(connection.insert_row(&insert)?.affected_rows, 1);
    assert_eq!(
        payloads(connection, "alice")?,
        vec![Value::Text("first".to_string())]
    );
    refusal(connection.insert_row(&insert), "was not applied");

    // An update by the whole key, then the same update of a row that is not
    // there: `IF EXISTS` refuses instead of creating it.
    let update = RowPatch::new(
        event_identity("alice"),
        "events".to_string(),
        Some(KEYSPACE.to_string()),
        vec![("payload".to_string(), Value::Text("second".to_string()))],
    );
    assert_eq!(connection.update_row(&update)?.affected_rows, 1);
    assert_eq!(
        payloads(connection, "alice")?,
        vec![Value::Text("second".to_string())]
    );
    let missing = RowPatch::new(
        event_identity("nobody"),
        "events".to_string(),
        Some(KEYSPACE.to_string()),
        vec![("payload".to_string(), Value::Text("never".to_string()))],
    );
    refusal(connection.update_row(&missing), "was not applied");

    // A delete by the whole key, then the same delete again.
    let delete = RowDelete::new(
        event_identity("alice"),
        "events".to_string(),
        Some(KEYSPACE.to_string()),
    );
    assert_eq!(connection.delete_row(&delete)?.affected_rows, 1);
    assert!(payloads(connection, "alice")?.is_empty());
    refusal(connection.delete_row(&delete), "was not applied");

    // A table whose details this session never read still edits: the first
    // edit reads the catalog once for its key, which is the cold-cache cost the
    // README states. `every_type` is the row the type test left behind.
    let cold = RowDelete::new(
        RowIdentity::composite(vec!["id".to_string()], vec![Value::Int(1)]),
        "every_type".to_string(),
        Some(KEYSPACE.to_string()),
    );
    assert_eq!(connection.delete_row(&cold)?.affected_rows, 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// Relations that refuse an edit
// ---------------------------------------------------------------------------

fn read_only_relations(connection: &dyn Connection) -> Result<(), DbError> {
    // A materialized view is maintained by its base table, so an edit of it is
    // refused and the refusal says where the row really lives.
    let view = RowPatch::new(
        RowIdentity::composite(
            vec!["who".to_string(), "bucket".to_string(), "at".to_string()],
            vec![
                Value::Text("alice".to_string()),
                Value::Int(i64::from(EVENTS_BUCKET)),
                Value::DateTime(event_time()),
            ],
        ),
        "events_by_who".to_string(),
        Some(KEYSPACE.to_string()),
        vec![("payload".to_string(), Value::Text("never".to_string()))],
    );
    match connection.update_row(&view) {
        Err(DbError::NotSupported(message)) => {
            assert!(
                message.contains("read-only"),
                "the refusal says a view is read-only: {message}"
            );
        }
        other => panic!("a view refuses a row edit, got {other:?}"),
    }

    // A counter changes by increment, never by assignment, so a row edit is
    // refused; the counter itself still increments through a statement.
    let counter = RowPatch::new(
        RowIdentity::composite(
            vec!["bucket".to_string(), "at".to_string()],
            vec![
                Value::Int(i64::from(EVENTS_BUCKET)),
                Value::DateTime(event_time()),
            ],
        ),
        "hits".to_string(),
        Some(KEYSPACE.to_string()),
        vec![("total".to_string(), Value::Int(5))],
    );
    match connection.update_row(&counter) {
        Err(DbError::NotSupported(message)) => {
            assert!(
                message.contains("counter"),
                "the refusal says a counter is not a row edit: {message}"
            );
        }
        other => panic!("a counter table refuses a row edit, got {other:?}"),
    }

    let mut increment = QueryRequest::new(format!(
        "UPDATE {KEYSPACE}.hits SET total = total + 1 WHERE bucket = ? AND at = ?"
    ));
    increment.params = vec![
        Value::Int(i64::from(EVENTS_BUCKET)),
        Value::DateTime(event_time()),
    ];
    connection.execute(&increment)?;

    let mut read = QueryRequest::new(format!(
        "SELECT total FROM {KEYSPACE}.hits WHERE bucket = ? AND at = ?"
    ));
    read.params = vec![
        Value::Int(i64::from(EVENTS_BUCKET)),
        Value::DateTime(event_time()),
    ];
    let total = connection.execute(&read)?;
    assert_eq!(total.rows.len(), 1);
    assert_eq!(total.rows[0][0], Value::Int(1));
    Ok(())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

fn formatted_message(error: &DbError) -> (&str, Option<&str>) {
    let formatted = error
        .formatted()
        .unwrap_or_else(|| panic!("the error carries a formatted body: {error:?}"));
    (formatted.message.as_str(), formatted.code.as_deref())
}

/// The server's own answer, in the form the toast and the audit row show it.
fn formatted_server_error(connection: &dyn Connection) -> Result<(), DbError> {
    let syntax = connection
        .execute(&QueryRequest::new(
            "SELEC release_version FROM system.local",
        ))
        .expect_err("a misspelled statement is a syntax error");
    let DbError::SyntaxError(_) = &syntax else {
        panic!("a misspelled statement is a SyntaxError, got {syntax:?}");
    };
    let (message, code) = formatted_message(&syntax);
    assert!(
        message.contains("line 1"),
        "the server's reason reaches the user: {message}"
    );
    assert_eq!(code, Some("0x2000"), "a CQL syntax error carries its code");

    let missing = connection
        .execute(&QueryRequest::new(format!(
            "SELECT * FROM {KEYSPACE}.not_a_table"
        )))
        .expect_err("a missing table is an error");
    assert!(
        matches!(missing, DbError::ObjectNotFound(_)),
        "a missing table is ObjectNotFound, got {missing:?}"
    );

    // A filter the server refuses answers with its own reason and code, which
    // is what tells a user why the statement will not run. `pages` carries no
    // index, so `label` can only be filtered with ALLOW FILTERING.
    let filtered = connection
        .execute(&QueryRequest::new(format!(
            "SELECT * FROM {KEYSPACE}.pages WHERE label = 'row-1'"
        )))
        .expect_err("a filter without ALLOW FILTERING is refused");
    assert!(
        matches!(filtered, DbError::QueryFailed(_)),
        "a refused filter is a query failure, got {filtered:?}"
    );
    let (message, code) = formatted_message(&filtered);
    assert!(
        message.contains("ALLOW FILTERING"),
        "the server's reason reaches the user: {message}"
    );
    assert_eq!(code, Some("0x2200"), "an invalid query carries its code");
    Ok(())
}

// ---------------------------------------------------------------------------
// The suites
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires Docker daemon"]
fn cassandra_live_connect_reads_the_release_version() -> Result<(), DbError> {
    containers::with_cassandra(|endpoint| {
        let driver = CassandraDriver::new();
        let profile = ConnectionProfile::new(
            "live-cassandra",
            DbConfig::Cassandra {
                contact_points: endpoint.host.clone(),
                port: endpoint.port,
                datacenter: String::new(),
                keyspace: String::new(),
                user: String::new(),
                ssl_mode: Some("disable".to_string()),
                ssl_root_cert_path: None,
                ssl_client_cert_path: None,
                ssl_client_key_path: None,
                ssh_tunnel: None,
                ssh_tunnel_profile_id: None,
            },
        );

        let connection =
            containers::retry_db_operation(Duration::from_secs(120), || -> Result<_, DbError> {
                let connection = driver.connect(&profile)?;
                connection.ping()?;
                Ok(connection)
            })?;

        assert_eq!(connection.kind(), DbKind::Cassandra);
        assert_release(connection.as_ref(), "5.")?;

        // A row limit is enforced by the driver, not by rewriting the query:
        // one row is kept, and the result says rows were dropped.
        let limited = connection.execute(&QueryRequest {
            limit: Some(1),
            ..QueryRequest::new("SELECT keyspace_name FROM system_schema.keyspaces")
        })?;
        assert_eq!(
            limited.rows.len(),
            1,
            "the server has more than one keyspace, so a limit of 1 keeps one row"
        );
        assert!(
            limited.rows_truncated(),
            "a result with rows the limit dropped is marked truncated"
        );

        create_fixture(connection.as_ref(), Server::Cassandra)?;
        schema_browse(connection.as_ref())?;
        drop_fixture(connection.as_ref())
    })
}

#[test]
#[ignore = "requires Docker daemon"]
fn cassandra_live_suite() -> Result<(), DbError> {
    containers::with_authenticated_cassandra(|endpoint| {
        let connection = connect(&profile(&endpoint, "disable", None))?;
        assert_release(connection.as_ref(), "5.")?;
        assert_password_is_required(&endpoint, connection.as_ref())?;
        create_fixture(connection.as_ref(), Server::Cassandra)?;
        run_suite(connection.as_ref())
    })
}

#[test]
#[ignore = "requires Docker daemon"]
fn scylla_live_suite() -> Result<(), DbError> {
    containers::with_scylla(|endpoint| {
        let connection = connect(&profile(&endpoint, "disable", None))?;
        assert_scylla_release(connection.as_ref())?;
        assert_password_is_required(&endpoint, connection.as_ref())?;
        create_fixture(connection.as_ref(), Server::Scylla)?;
        run_suite(connection.as_ref())
    })
}

#[test]
#[ignore = "requires Docker daemon, the local scylladb/scylla:6.2 image and the openssl command"]
fn scylla_live_tls_modes() -> Result<(), DbError> {
    let certificates = Certificates::make();
    containers::with_scylla_tls(certificates.folder(), |endpoint| -> Result<(), DbError> {
        let verified = tls_modes_are_enforced(&endpoint, &certificates)?;
        // A connection over TLS reads and writes like any other, fixture and
        // all.
        create_fixture(verified.as_ref(), Server::Scylla)?;
        drop_fixture(verified.as_ref())?;
        Ok(())
    })?;
    Ok(())
}

/// What a TLS-only port proves: `require` encrypts and verifies nothing, so it
/// works without a CA; `disable` must fail, or the port is not TLS-only and the
/// other modes would prove nothing; and the two verifying modes must accept the
/// authority that signed the node and refuse one that did not.
///
/// Returns the `verify-full` connection, the strongest mode, for the suite that
/// follows.
fn tls_modes_are_enforced(
    endpoint: &containers::CassandraEndpoint,
    certificates: &Certificates,
) -> Result<Box<dyn Connection>, DbError> {
    let encrypted = connect(&profile(endpoint, "require", None))?;
    assert_scylla_release(encrypted.as_ref())?;

    let plain = connect_attempt(
        &profile(endpoint, "disable", None),
        Some(containers::CQL_TEST_PASSWORD),
    )
    .err();
    assert!(
        matches!(plain, Some(DbError::ConnectionFailed(_))),
        "a TLS-only port refuses a plain connection: {plain:?}"
    );

    verifying_modes_accept_the_authority(endpoint, certificates)
}

/// `verify-ca` checks the chain only, and `verify-full` also matches the
/// address the driver dials. The fixture certificate carries `IP:127.0.0.1`,
/// which is that address, so `verify-full` connects; an authority that signed
/// nothing here fails under either mode, or nothing is being verified.
fn verifying_modes_accept_the_authority(
    endpoint: &containers::CassandraEndpoint,
    certificates: &Certificates,
) -> Result<Box<dyn Connection>, DbError> {
    for mode in ["verify-ca", "verify-full"] {
        let untrusted = connect_attempt(
            &profile(endpoint, mode, Some(&certificates.other_ca())),
            Some(containers::CQL_TEST_PASSWORD),
        )
        .err();
        assert!(
            matches!(untrusted, Some(DbError::ConnectionFailed(_))),
            "{mode} refuses a chain signed by another authority: {untrusted:?}"
        );
    }

    let authority = certificates.authority();
    let chain_only = connect(&profile(endpoint, "verify-ca", Some(&authority)))?;
    assert_scylla_release(chain_only.as_ref())?;

    let full = connect(&profile(endpoint, "verify-full", Some(&authority)))?;
    assert_eq!(full.kind(), DbKind::Cassandra);
    assert_scylla_release(full.as_ref())?;
    Ok(full)
}

// ---------------------------------------------------------------------------
// Certificates for the TLS suite
// ---------------------------------------------------------------------------

/// A throwaway authority and a certificate for the node, made when the test
/// runs and removed with it. Nothing here is a secret of ours: the key signs
/// one container's certificate for a few minutes.
const MAKE_CERTIFICATES: &str = r#"set -e
new_key="-newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes"
self_signed() {
  openssl req -x509 $new_key -days 1 -keyout "$1.key" -out "$1.crt" -subj "/CN=$1"
}
self_signed ca
self_signed other-ca
openssl req $new_key -keyout server.key -out server.csr -subj "/CN=datatab-live"
printf 'subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > server.ext
openssl x509 -req -days 1 -CAcreateserial -CA ca.crt -CAkey ca.key \
  -in server.csr -extfile server.ext -out server.crt
"#;

struct Certificates {
    folder: PathBuf,
}

impl Certificates {
    fn make() -> Self {
        let scratch = std::env::var_os("DATATAB_TEST_SCRATCH")
            .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
        let folder = scratch.join(format!("cassandra-tls-{}", std::process::id()));
        remove_folder(&folder);
        fs::create_dir_all(&folder).expect("create the certificate folder");
        let output = Command::new("sh")
            .args(["-c", MAKE_CERTIFICATES])
            .current_dir(&folder)
            .output()
            .expect("start openssl");
        assert!(
            output.status.success(),
            "make certificates: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self { folder }
    }

    fn folder(&self) -> &Path {
        &self.folder
    }

    /// The authority that signed the node's certificate.
    fn authority(&self) -> PathBuf {
        self.folder.join("ca.crt")
    }

    /// An authority that did not.
    fn other_ca(&self) -> PathBuf {
        self.folder.join("other-ca.crt")
    }
}

impl Drop for Certificates {
    fn drop(&mut self) {
        remove_folder(&self.folder);
    }
}

fn remove_folder(folder: &Path) {
    if let Err(error) = fs::remove_dir_all(folder)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("failed to remove {}: {error}", folder.display());
    }
}
