//! Unit tests for the Cassandra driver's parts that need no cluster.

use std::sync::Arc;

use datatab_core::{
    AccessKind, AssignmentValue, BoolOp, ColumnAssignment, Comparator, ConnectionProfile,
    DangerousQueryKind, DbConfig, DbDriver, DbError, DbKind, ExecutionClassification, FilterNode,
    JoinKind, JoinOn, JoinStep, LanguageService, LiteralValue, MutationKind, MutationRequest,
    OrderByColumn, Pagination, Predicate, PredicateValue, Projection, QueryGenError,
    QueryGenerator, QueryLanguage, QueryResult, ReadTemplateOperation, ReadTemplateRequest,
    RowDelete, RowInsert, RowPatch, SortEntry, SourceTable, SqlDialect, TableBrowseRequest,
    TableRef, ValidationResult, Value, VisualMutationSpec, VisualQuerySpec, VisualSortDirection,
};
use scylla::frame::response::result::{CollectionType, ColumnType, NativeType};
use scylla::response::PagingState;
use scylla::value::{CqlDate, CqlTime, CqlValue};

use crate::connection::{browse_cql, browse_page_number, row_count_refusal};
use crate::driver::{CASSANDRA_FORM, CassandraDriver, METADATA, SSL_MODES, parse_port};
use crate::edit::{
    RelationKind, composite_identity, delete_statement, insert_statement, not_applied,
    update_statement,
};
use crate::generator::{DIALECT, GENERATOR};
use crate::language_service::{
    ActiveKeyspace, CqlLanguageService, statement_count as cql_statement_count,
};
use crate::paging::{BrowseKey, MAX_CACHED_PAGES, MAX_PAGES_WALKED, PagingCache, read_grid_page};
use crate::tls::TlsMode;
use crate::types::{column_kind, column_type_name, value_from_cql, values_to_cql};

// =============================================================================
// Driver surface
// =============================================================================

#[test]
fn metadata_names_one_driver_for_cassandra_and_scylladb() {
    assert_eq!(METADATA.id, "cassandra");
    assert_eq!(METADATA.display_name, "Cassandra / ScyllaDB");
    assert_eq!(METADATA.query_language, QueryLanguage::Cql);
    assert_eq!(METADATA.default_port, Some(9042));
    assert_eq!(METADATA.uri_scheme, "cassandra");
    assert!(
        METADATA
            .syntax
            .as_ref()
            .is_some_and(|syntax| syntax.supports_schemas)
    );
    // CQL has no BEGIN/COMMIT, so the driver advertises no transactions.
    assert!(METADATA.transactions.is_none());
}

#[test]
fn tls_modes_use_the_words_the_other_drivers_use() {
    let ids: Vec<&str> = SSL_MODES.iter().map(|mode| mode.id).collect();
    assert_eq!(ids, vec!["disable", "require", "verify-ca", "verify-full"]);
    for mode in SSL_MODES {
        assert_eq!(
            TlsMode::parse(mode.id).expect("known mode").as_id(),
            mode.id
        );
    }
    assert!(TlsMode::VerifyCa.requires_root_cert());
    assert!(TlsMode::VerifyFull.requires_root_cert());
    assert!(!TlsMode::Require.requires_root_cert());
    assert!(TlsMode::parse("verify-none").is_err());
}

#[test]
fn build_config_trims_and_round_trips_every_field() {
    let driver = CassandraDriver::new();
    let mut values = datatab_core::FormValues::new();
    values.insert(
        "contact_points".to_string(),
        " 10.0.0.1, 10.0.0.2 ".to_string(),
    );
    values.insert("port".to_string(), "9043".to_string());
    values.insert("datacenter".to_string(), " dc1 ".to_string());
    values.insert("keyspace".to_string(), " app ".to_string());
    values.insert("user".to_string(), " reader ".to_string());

    let config = driver.build_config(&values).expect("valid form values");
    let DbConfig::Cassandra {
        contact_points,
        port,
        datacenter,
        keyspace,
        user,
        ssl_mode,
        ssl_root_cert_path,
        ssl_client_cert_path,
        ssh_tunnel,
        ..
    } = config.clone()
    else {
        panic!("build_config must produce a Cassandra config");
    };
    assert_eq!(contact_points, "10.0.0.1,10.0.0.2");
    assert_eq!(port, 9043);
    assert_eq!(datacenter, "dc1");
    assert_eq!(keyspace, "app");
    assert_eq!(user, "reader");
    // TLS belongs to the shared Transport control, which writes the mode and
    // the paths into the config after build_config returns.
    assert_eq!(ssl_mode.as_deref(), Some("disable"));
    assert!(ssl_root_cert_path.is_none());
    assert!(ssl_client_cert_path.is_none());
    assert!(ssh_tunnel.is_none());
    assert_eq!(config.kind(), DbKind::Cassandra);

    let back = driver.extract_values(&config);
    assert_eq!(
        back.get("contact_points").map(String::as_str),
        Some("10.0.0.1,10.0.0.2")
    );
    assert_eq!(back.get("port").map(String::as_str), Some("9043"));
    assert_eq!(back.get("datacenter").map(String::as_str), Some("dc1"));
    assert_eq!(back.get("keyspace").map(String::as_str), Some("app"));
    assert_eq!(back.get("user").map(String::as_str), Some("reader"));
    // The Transport control reads TLS from the config, so the form values carry
    // no copy of it that could disagree.
    assert!(!back.contains_key("ssl_mode"));
    assert!(!back.contains_key("ssl_root_cert_path"));
}

#[test]
fn tls_has_one_control_so_the_form_declares_no_tls_field() {
    let fields: Vec<&str> = CASSANDRA_FORM
        .tabs
        .iter()
        .flat_map(|tab| tab.sections.iter())
        .flat_map(|section| section.fields.iter())
        .map(|field| field.id.as_str())
        .collect();
    assert!(!fields.contains(&"ssl_mode"));
    assert!(!fields.contains(&"ssl_root_cert_path"));

    // The Main tab declares the password field, which is what draws the secret
    // input even though a password is optional.
    assert!(fields.contains(&"password"));

    // The shared Transport control needs the modes and the certificate pickers.
    assert!(METADATA.ssl_modes.is_some());
    let cert_fields = METADATA
        .ssl_cert_fields
        .as_ref()
        .expect("Cassandra takes a CA file and a client pair");
    assert!(cert_fields.root_cert);
    assert!(cert_fields.client_cert);
    assert_eq!(
        cert_fields.visible_for_mode("verify-ca"),
        (true, true),
        "verify-ca uses both the CA and a client pair"
    );
    assert_eq!(
        cert_fields.visible_for_mode("require"),
        (false, true),
        "require encrypts without verifying, so only a client pair applies"
    );
    assert_eq!(cert_fields.visible_for_mode("disable"), (false, false));
}

#[test]
fn contact_points_are_hosts_not_urls_and_carry_no_port() {
    let driver = CassandraDriver::new();
    for contact_points in ["cassandra://db1", "db1:9042", "db1 / var", "db1?dc=dc1", ""] {
        let profile = profile_with(config_with(|config| {
            if let DbConfig::Cassandra {
                contact_points: value,
                ..
            } = config
            {
                *value = contact_points.to_string();
            }
        }));
        assert!(
            driver.validate_profile(&profile).is_err(),
            "'{contact_points}' must be refused"
        );
    }

    // IPv6 addresses keep their colons.
    let profile = profile_with(config_with(|config| {
        if let DbConfig::Cassandra {
            contact_points: value,
            ..
        } = config
        {
            *value = "::1".to_string();
        }
    }));
    assert!(driver.validate_profile(&profile).is_ok());
}

#[test]
fn port_must_be_a_usable_number() {
    assert_eq!(parse_port(None).expect("an empty port is 9042"), 9042);
    assert_eq!(
        parse_port(Some(&String::new())).expect("an empty port is 9042"),
        9042
    );
    assert_eq!(
        parse_port(Some(&"9142".to_string())).expect("a real port is accepted"),
        9142
    );
    for bad in ["0", "70000", "ninety", "-1", "9042x"] {
        assert!(
            parse_port(Some(&bad.to_string())).is_err(),
            "'{bad}' must be refused"
        );
    }
}

#[test]
fn an_unknown_tls_mode_is_refused() {
    let driver = CassandraDriver::new();
    let profile = profile_with(config_with(|config| {
        if let DbConfig::Cassandra { ssl_mode, .. } = config {
            *ssl_mode = Some("verify-sideways".to_string());
        }
    }));
    let error = driver
        .validate_profile(&profile)
        .expect_err("an unknown TLS mode must be refused");
    assert!(error.to_string().contains("verify-sideways"));
}

#[test]
fn a_client_certificate_needs_its_key_and_a_tls_mode() {
    let driver = CassandraDriver::new();

    let certificate_only = profile_with(config_with(|config| {
        if let DbConfig::Cassandra {
            ssl_client_cert_path,
            ..
        } = config
        {
            *ssl_client_cert_path = Some("/etc/client.pem".to_string());
        }
    }));
    assert!(driver.validate_profile(&certificate_only).is_err());

    let key_only = profile_with(config_with(|config| {
        if let DbConfig::Cassandra {
            ssl_client_key_path,
            ..
        } = config
        {
            *ssl_client_key_path = Some("/etc/client.key".to_string());
        }
    }));
    assert!(driver.validate_profile(&key_only).is_err());

    let both_without_tls = profile_with(config_with(|config| {
        if let DbConfig::Cassandra {
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        } = config
        {
            *ssl_client_cert_path = Some("/etc/client.pem".to_string());
            *ssl_client_key_path = Some("/etc/client.key".to_string());
        }
    }));
    assert!(driver.validate_profile(&both_without_tls).is_err());

    let both_with_tls = profile_with(config_with(|config| {
        if let DbConfig::Cassandra {
            ssl_mode,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        } = config
        {
            *ssl_mode = Some("verify-full".to_string());
            *ssl_client_cert_path = Some("/etc/client.pem".to_string());
            *ssl_client_key_path = Some("/etc/client.key".to_string());
        }
    }));
    assert!(driver.validate_profile(&both_with_tls).is_ok());
}

#[test]
fn proxy_and_ssh_tunnels_are_refused_and_direct_access_is_accepted() {
    let driver = CassandraDriver::new();
    let mut profile = profile_with(config_with(|_| {}));
    let direct = driver.validate_profile(&profile);
    assert!(direct.is_ok());

    profile.access_kind = Some(AccessKind::Ssh {
        ssh_tunnel_profile_id: uuid::Uuid::nil(),
    });
    assert!(driver.validate_profile(&profile).is_err());

    profile.access_kind = Some(AccessKind::Proxy {
        proxy_profile_id: uuid::Uuid::nil(),
    });
    assert!(driver.validate_profile(&profile).is_err());

    profile.access_kind = Some(AccessKind::Managed {
        provider: "example".to_string(),
        params: Default::default(),
    });
    assert!(driver.validate_profile(&profile).is_err());
}

#[test]
fn the_driver_reports_its_key_and_kind() {
    let driver = CassandraDriver::new();
    assert_eq!(driver.kind(), DbKind::Cassandra);
    assert_eq!(driver.driver_key(), "builtin:cassandra");
    assert_eq!(driver.metadata().id, "cassandra");
    // Open-source clusters often allow anonymous access.
    assert!(!driver.requires_password());
}

// =============================================================================
// Values and types
// =============================================================================

#[test]
fn scalar_cql_values_map_to_the_grid_values() {
    assert_eq!(
        value_from_cql(&CqlValue::Text("hello".to_string())),
        Value::Text("hello".to_string())
    );
    assert_eq!(
        value_from_cql(&CqlValue::Ascii("hello".to_string())),
        Value::Text("hello".to_string())
    );
    assert_eq!(value_from_cql(&CqlValue::Boolean(true)), Value::Bool(true));
    assert_eq!(value_from_cql(&CqlValue::Int(7)), Value::Int(7));
    assert_eq!(value_from_cql(&CqlValue::BigInt(-9)), Value::Int(-9));
    assert_eq!(value_from_cql(&CqlValue::SmallInt(3)), Value::Int(3));
    assert_eq!(value_from_cql(&CqlValue::TinyInt(1)), Value::Int(1));
    assert_eq!(value_from_cql(&CqlValue::Float(1.5)), Value::Float(1.5));
    assert_eq!(
        value_from_cql(&CqlValue::Double(-2.25)),
        Value::Float(-2.25)
    );
    assert_eq!(
        value_from_cql(&CqlValue::Blob(vec![1, 2, 3])),
        Value::Bytes(vec![1, 2, 3])
    );
    assert_eq!(
        value_from_cql(&CqlValue::Inet("127.0.0.1".parse().expect("ip"))),
        Value::Text("127.0.0.1".to_string())
    );
    assert_eq!(
        value_from_cql(&CqlValue::Time(CqlTime(3_600_000_000_000))),
        Value::Time(chrono::NaiveTime::from_hms_opt(1, 0, 0).expect("time"))
    );
    assert_eq!(
        value_from_cql(&CqlValue::Date(CqlDate(1 << 31))),
        Value::Date(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("date"))
    );
    assert_eq!(
        value_from_cql(&CqlValue::Timestamp(scylla::value::CqlTimestamp(0))),
        Value::DateTime(chrono::DateTime::from_timestamp(0, 0).expect("epoch"))
    );
    assert_eq!(
        value_from_cql(&CqlValue::Uuid(uuid::Uuid::nil())),
        Value::Text(uuid::Uuid::nil().to_string())
    );
}

#[test]
fn decimal_renders_as_exact_text_and_varint_keeps_its_integer_kind() {
    // A varint that fits an integer stays a number, so the Integer column kind
    // and the cell agree.
    assert_eq!(
        value_from_cql(&CqlValue::Varint(
            scylla::value::CqlVarint::from_signed_bytes_be_slice(&[0x01, 0x00])
        )),
        Value::Int(256)
    );
    assert_eq!(
        value_from_cql(&CqlValue::Varint(
            scylla::value::CqlVarint::from_signed_bytes_be_slice(&[0xFF])
        )),
        Value::Int(-1)
    );
    // Past `i64` the exact text is kept instead of being rounded.
    assert_eq!(
        value_from_cql(&CqlValue::Varint(
            scylla::value::CqlVarint::from_signed_bytes_be_slice(&[
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ])
        )),
        Value::Decimal("18446744073709551616".to_string())
    );
}

#[test]
fn decimal_text_keeps_the_scale_the_server_sent() {
    assert_eq!(
        crate::types::format_decimal(1250, 2).as_deref(),
        Some("12.50")
    );
    assert_eq!(
        crate::types::format_decimal(-1250, 2).as_deref(),
        Some("-12.50")
    );
    assert_eq!(crate::types::format_decimal(5, 3).as_deref(), Some("0.005"));
    assert_eq!(crate::types::format_decimal(42, 0).as_deref(), Some("42"));
    assert_eq!(
        crate::types::format_decimal(42, -2).as_deref(),
        Some("4200")
    );
    // A scale past what a cell can hold is undecodable, not a gigabyte string.
    assert!(crate::types::format_decimal(1, i32::MIN).is_none());
    assert!(crate::types::format_decimal(1, i32::MAX).is_none());
}

#[test]
fn collections_map_to_arrays_and_nested_types_keep_their_shape() {
    assert_eq!(
        value_from_cql(&CqlValue::List(vec![CqlValue::Int(1), CqlValue::Int(2)])),
        Value::Array(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        value_from_cql(&CqlValue::Tuple(vec![Some(CqlValue::Int(1)), None,])),
        Value::Array(vec![Value::Int(1), Value::Null])
    );

    let Value::Document(document) = value_from_cql(&CqlValue::Map(vec![(
        CqlValue::Text("k".to_string()),
        CqlValue::Int(1),
    )])) else {
        panic!("a CQL map becomes a document");
    };
    assert_eq!(document.get("k"), Some(&Value::Int(1)));
}

#[test]
fn column_names_and_kinds_follow_the_cql_type() {
    assert_eq!(
        column_type_name(&ColumnType::Native(NativeType::Int)),
        "int"
    );
    assert_eq!(
        column_type_name(&ColumnType::Collection {
            frozen: false,
            typ: CollectionType::List(Box::new(ColumnType::Native(NativeType::Int))),
        }),
        "list<int>"
    );
    assert_eq!(
        column_type_name(&ColumnType::Collection {
            frozen: true,
            typ: CollectionType::Map(
                Box::new(ColumnType::Native(NativeType::Text)),
                Box::new(ColumnType::Native(NativeType::BigInt)),
            ),
        }),
        "frozen<map<text, bigint>>"
    );
    assert_eq!(
        column_type_name(&ColumnType::Tuple(vec![
            ColumnType::Native(NativeType::Int),
            ColumnType::Native(NativeType::Text),
        ])),
        "tuple<int, text>"
    );

    assert_eq!(
        datatab_core::ColumnKind::Timestamp,
        column_kind(&ColumnType::Native(NativeType::Timestamp))
    );
    assert_eq!(
        datatab_core::ColumnKind::Integer,
        column_kind(&ColumnType::Native(NativeType::Counter))
    );
    assert_eq!(
        datatab_core::ColumnKind::Float,
        column_kind(&ColumnType::Native(NativeType::Decimal))
    );
    assert_eq!(
        datatab_core::ColumnKind::Text,
        column_kind(&ColumnType::Native(NativeType::Ascii))
    );
    // Design §3: identifiers and addresses are text, so the chart engine can
    // label them; a collection has no single kind.
    assert_eq!(
        datatab_core::ColumnKind::Text,
        column_kind(&ColumnType::Native(NativeType::Uuid))
    );
    assert_eq!(
        datatab_core::ColumnKind::Text,
        column_kind(&ColumnType::Native(NativeType::Time))
    );
    assert_eq!(
        datatab_core::ColumnKind::Text,
        column_kind(&ColumnType::Native(NativeType::Duration))
    );
    assert_eq!(
        datatab_core::ColumnKind::Unknown,
        column_kind(&ColumnType::Native(NativeType::Boolean))
    );
    assert_eq!(
        datatab_core::ColumnKind::Unknown,
        column_kind(&ColumnType::Collection {
            frozen: false,
            typ: CollectionType::List(Box::new(ColumnType::Native(NativeType::Int))),
        })
    );
}

#[test]
fn bound_parameters_keep_their_type_and_refuse_what_this_part_cannot_bind() {
    assert_eq!(
        values_to_cql(&[Value::Null]).expect("null binds"),
        vec![None]
    );
    assert_eq!(
        values_to_cql(&[Value::Int(7)]).expect("int binds"),
        vec![Some(CqlValue::Int(7))]
    );
    assert_eq!(
        values_to_cql(&[Value::Int(i64::from(i32::MAX) + 1)]).expect("wide int binds"),
        vec![Some(CqlValue::BigInt(i64::from(i32::MAX) + 1))]
    );
    assert_eq!(
        values_to_cql(&[Value::Text("x".to_string())]).expect("text binds"),
        vec![Some(CqlValue::Text("x".to_string()))]
    );
    assert_eq!(
        values_to_cql(&[Value::Time(
            chrono::NaiveTime::from_hms_opt(0, 0, 1).expect("time")
        )])
        .expect("time binds"),
        vec![Some(CqlValue::Time(CqlTime(1_000_000_000)))]
    );

    assert!(values_to_cql(&[Value::Array(vec![Value::Int(1)])]).is_err());
    assert!(values_to_cql(&[Value::Unsupported("blob".to_string())]).is_err());
}

#[test]
fn capabilities_claim_only_what_this_part_does() {
    let capabilities = METADATA.capabilities;
    // Paging, filtering, TLS, the request row limit, views, indexes and user
    // types are implemented, and the driver speaks CQL with a password and
    // keyspaces.
    for claimed in [
        datatab_core::DriverCapabilities::SSL,
        datatab_core::DriverCapabilities::REQUEST_ROW_LIMIT,
        datatab_core::DriverCapabilities::AUTHENTICATION,
        datatab_core::DriverCapabilities::MULTIPLE_DATABASES,
        datatab_core::DriverCapabilities::VIEWS,
        datatab_core::DriverCapabilities::INDEXES,
        datatab_core::DriverCapabilities::CUSTOM_TYPES,
        datatab_core::DriverCapabilities::PAGINATION,
        datatab_core::DriverCapabilities::FILTERING,
    ] {
        assert!(capabilities.contains(claimed), "{claimed:?} is claimed");
    }
    // Each of these arrives with the part that implements it, or never.
    for missing in [
        // CQL orders a partition by its clustering columns only, so the grid
        // sorts this driver's tables in memory (ruling 4).
        datatab_core::DriverCapabilities::SORTING,
        // CQL has no BEGIN/COMMIT: row edits are lightweight transactions.
        datatab_core::DriverCapabilities::TRANSACTIONS,
    ] {
        assert!(
            !capabilities.contains(missing),
            "{missing:?} is not claimed"
        );
    }
    // The browse pages through a server paging state, and no statement carries
    // an OFFSET.
    let query = METADATA.query.as_ref().expect("query capabilities");
    assert_eq!(
        query.pagination,
        vec![datatab_core::PaginationStyle::PageToken]
    );
    assert!(!query.supports_offset);
    assert!(!query.supports_order_by);
    assert!(!query.supports_joins);
    assert_eq!(query.order_by_mode, datatab_core::OrderByMode::None);
    // The generator renders `!=` and OR groups, so both are declared.
    for operator in [
        datatab_core::WhereOperator::Ne,
        datatab_core::WhereOperator::Or,
    ] {
        assert!(
            query.where_operators.contains(&operator),
            "{operator:?} is rendered by render_filter and must be declared"
        );
    }
    // CQL is not SQL, so the transfer wizard has no SQL DDL to emit for it.
    assert_eq!(
        METADATA.transfer_family,
        datatab_core::TransferFamily::Incompatible
    );
}

#[test]
fn the_page_size_follows_the_budget_and_probes_one_row_past_it() {
    assert_eq!(crate::connection::next_page_size(0, None), 500);
    assert_eq!(crate::connection::next_page_size(9_000, None), 500);
    assert_eq!(crate::connection::next_page_size(0, Some(25)), 25);
    assert_eq!(crate::connection::next_page_size(100, Some(500)), 400);
    assert_eq!(crate::connection::next_page_size(0, Some(usize::MAX)), 500);
    // The budget is met and the server may hold more: one row is asked for so a
    // result that ends on the budget is not reported as truncated.
    assert_eq!(crate::connection::next_page_size(25, Some(25)), 1);
    assert_eq!(crate::connection::next_page_size(0, Some(0)), 1);
}

#[test]
fn an_ipv6_contact_point_is_bracketed_for_the_crate() {
    assert_eq!(crate::session::node_address("db1", 9042), "db1:9042");
    assert_eq!(
        crate::session::node_address("10.0.0.1", 9043),
        "10.0.0.1:9043"
    );
    assert_eq!(crate::session::node_address("::1", 9042), "[::1]:9042");
    assert_eq!(crate::session::node_address("[::1]", 9042), "[::1]:9042");
}

// =============================================================================
// Schema browse
// =============================================================================

#[test]
fn the_users_keyspaces_are_listed_before_the_servers_own() {
    let mut names = vec![
        "system_auth".to_string(),
        "shop".to_string(),
        "system".to_string(),
        "analytics".to_string(),
        "system_schema".to_string(),
    ];

    crate::schema::sort_keyspaces(&mut names);

    assert_eq!(
        names,
        vec![
            "analytics",
            "shop",
            "system",
            "system_auth",
            "system_schema"
        ]
    );
    assert!(crate::schema::is_system_keyspace("system"));
    assert!(crate::schema::is_system_keyspace("system_traces"));
    // Only the exact name and the `system_` prefix are the server's: a user
    // keyspace that merely starts with the letters stays a user's.
    assert!(!crate::schema::is_system_keyspace("systems"));
}

#[test]
fn a_table_lists_its_primary_key_first_with_partition_before_clustering() {
    // Rows in the shape `system_schema.columns` returns: table, column, kind,
    // position, type, clustering order.
    let rows = vec![
        column_row("events", "payload", "regular", -1, "text", "none"),
        column_row("events", "at", "clustering", 1, "timestamp", "desc"),
        column_row("events", "bucket", "partition_key", 0, "int", "none"),
        column_row("events", "who", "clustering", 0, "text", "asc"),
        column_row("events", "note", "static", -1, "text", "none"),
    ];

    let layout = crate::schema::columns_from_rows(&rows).expect("a readable catalog row");
    let (columns, hints) = (layout.columns, layout.hints);

    let ordered: Vec<&str> = columns.iter().map(|column| column.name.as_str()).collect();
    assert_eq!(ordered, vec!["bucket", "who", "at", "note", "payload"]);

    let key_columns: Vec<&str> = columns
        .iter()
        .filter(|column| column.is_primary_key)
        .map(|column| column.name.as_str())
        .collect();
    assert_eq!(key_columns, vec!["bucket", "who", "at"]);

    // CQL has no `NOT NULL`: the key refuses a null and nothing else does.
    assert!(
        columns
            .iter()
            .filter(|column| column.is_primary_key)
            .all(|column| !column.nullable)
    );
    assert!(
        columns
            .iter()
            .filter(|column| !column.is_primary_key)
            .all(|column| column.nullable)
    );

    let hint = |label: &str| {
        hints
            .iter()
            .find(|hint| hint.label == label)
            .unwrap_or_else(|| panic!("a {label} hint is present"))
            .clone()
    };
    assert_eq!(hint("Partition key").columns, vec!["bucket"]);
    assert_eq!(hint("Clustering key").columns, vec!["who", "at"]);
    assert_eq!(hint("Static column").columns, vec!["note"]);
    // Only the descending part of the clustering order earns a hint row.
    assert_eq!(hint("Clustering order").columns, vec!["at"]);
    assert_eq!(
        hint("Clustering order").detail.as_deref(),
        Some("descending")
    );
}

#[test]
fn scylladb_writes_the_clustering_order_in_upper_case_and_keeps_its_hint() {
    // What ScyllaDB 6.2 returns: `DESC`/`ASC`/`NONE`. The same table must read
    // the same way on both servers, so the hint is not allowed to vanish here.
    let rows = vec![
        column_row("events", "at", "clustering", 1, "timestamp", "DESC"),
        column_row("events", "bucket", "partition_key", 0, "int", "NONE"),
        column_row("events", "who", "clustering", 0, "text", "ASC"),
    ];

    let layout = crate::schema::columns_from_rows(&rows).expect("a readable catalog row");
    let order = layout
        .hints
        .iter()
        .find(|hint| hint.label == "Clustering order")
        .unwrap_or_else(|| panic!("a Clustering order hint is present: {:?}", layout.hints));
    assert_eq!(order.columns, vec!["at"]);
    assert_eq!(order.detail.as_deref(), Some("descending"));
}

#[test]
fn a_table_without_a_static_or_descending_column_gets_no_empty_hint() {
    let rows = vec![
        column_row("plain", "id", "partition_key", 0, "uuid", "none"),
        column_row("plain", "value", "regular", -1, "text", "none"),
    ];

    let layout = crate::schema::columns_from_rows(&rows).expect("a readable catalog row");
    let (columns, hints) = (layout.columns, layout.hints);

    assert_eq!(columns.len(), 2);
    let labels: Vec<&str> = hints.iter().map(|hint| hint.label.as_str()).collect();
    assert_eq!(labels, vec!["Partition key"]);
}

#[test]
fn a_catalog_row_of_the_wrong_shape_fails_instead_of_dropping_a_column() {
    // `kind` arrives as an integer, which is not the shape `system_schema`
    // documents: every column would quietly become a regular column if the
    // driver defaulted instead of failing.
    let rows = vec![vec![
        Value::Text("events".to_string()),
        Value::Text("bucket".to_string()),
        Value::Int(0),
        Value::Int(0),
        Value::Text("int".to_string()),
        Value::Text("none".to_string()),
    ]];

    let outcome = crate::schema::columns_from_rows(&rows);

    assert!(
        matches!(outcome, Err(datatab_core::DbError::QueryFailed(_))),
        "a malformed catalog row is an error, got {outcome:?}"
    );
}

#[test]
fn a_column_of_an_unknown_kind_is_listed_without_being_marked_as_a_key() {
    // CQL has four column kinds today. A fifth must not stop the browse, and it
    // must not claim a place in the key either: the column is listed, unmarked.
    let rows = vec![
        column_row("events", "bucket", "partition_key", 0, "int", "none"),
        column_row(
            "events",
            "later",
            "a_kind_from_the_future",
            -1,
            "text",
            "none",
        ),
    ];

    let layout = crate::schema::columns_from_rows(&rows).expect("a readable catalog row");
    let (columns, hints) = (layout.columns, layout.hints);

    let later = columns
        .iter()
        .find(|column| column.name == "later")
        .expect("the unknown kind is still listed");
    assert!(!later.is_primary_key);
    assert!(later.nullable);
    assert!(
        !hints
            .iter()
            .any(|hint| hint.columns.contains(&"later".to_string())),
        "an unknown kind claims no key role: {hints:?}"
    );
}

#[test]
fn an_index_target_is_the_column_the_secondary_index_covers() {
    assert_eq!(
        crate::schema::target_column_name("values(country)").as_deref(),
        Some("country")
    );
    assert_eq!(
        crate::schema::target_column_name("keys(\"Country\")").as_deref(),
        Some("Country")
    );
    assert_eq!(
        crate::schema::target_column_name("entries(values)").as_deref(),
        Some("values")
    );
    // A custom index may hold the bare column.
    assert_eq!(
        crate::schema::target_column_name("country").as_deref(),
        Some("country")
    );
    assert_eq!(crate::schema::target_column_name("   "), None);
    assert_eq!(crate::schema::target_column_name("values()"), None);
}

#[test]
fn every_cql_native_type_has_a_chart_kind() {
    use datatab_core::ColumnKind;

    let expected = [
        (NativeType::Ascii, ColumnKind::Text),
        (NativeType::Text, ColumnKind::Text),
        (NativeType::Inet, ColumnKind::Text),
        (NativeType::Uuid, ColumnKind::Text),
        (NativeType::Timeuuid, ColumnKind::Text),
        (NativeType::Time, ColumnKind::Text),
        (NativeType::Duration, ColumnKind::Text),
        (NativeType::Int, ColumnKind::Integer),
        (NativeType::BigInt, ColumnKind::Integer),
        (NativeType::SmallInt, ColumnKind::Integer),
        (NativeType::TinyInt, ColumnKind::Integer),
        (NativeType::Counter, ColumnKind::Integer),
        (NativeType::Varint, ColumnKind::Integer),
        (NativeType::Float, ColumnKind::Float),
        (NativeType::Double, ColumnKind::Float),
        (NativeType::Decimal, ColumnKind::Float),
        (NativeType::Timestamp, ColumnKind::Timestamp),
        (NativeType::Date, ColumnKind::Timestamp),
        (NativeType::Blob, ColumnKind::Unknown),
        (NativeType::Boolean, ColumnKind::Unknown),
    ];
    for (native, kind) in expected {
        let kind_of = column_kind(&ColumnType::Native(native.clone()));
        assert_eq!(kind, kind_of, "{native:?}");
    }

    // A collection, a vector and a tuple have no single kind to plot.
    assert_eq!(
        ColumnKind::Unknown,
        column_kind(&ColumnType::Collection {
            frozen: true,
            typ: CollectionType::Map(
                Box::new(ColumnType::Native(NativeType::Text)),
                Box::new(ColumnType::Native(NativeType::Int)),
            ),
        })
    );
    assert_eq!(
        ColumnKind::Unknown,
        column_kind(&ColumnType::Vector {
            typ: Box::new(ColumnType::Native(NativeType::Float)),
            dimensions: 3,
        })
    );
}

// =============================================================================
// CQL language service
// =============================================================================

fn classify(query: &str) -> ExecutionClassification {
    CqlLanguageService::new(ActiveKeyspace::default())
        .classify_execution(query)
        .expect("the CQL service classifies every statement")
}

/// A service whose session names no keyspace, which is enough for everything
/// that reads the statement text alone.
fn fresh() -> CqlLanguageService {
    CqlLanguageService::new(ActiveKeyspace::default())
}

/// A service whose session runs in `keyspace`, plus a handle on the lock the
/// connection shares, so a test can move the session as `USE` does.
fn service_in_session(keyspace: Option<&str>) -> (CqlLanguageService, ActiveKeyspace) {
    let active = ActiveKeyspace::default();
    set_session_keyspace(&active, keyspace);
    (CqlLanguageService::new(Arc::clone(&active)), active)
}

fn set_session_keyspace(active: &ActiveKeyspace, keyspace: Option<&str>) {
    *active.write().expect("a lock the test owns") = keyspace.map(str::to_string);
}

fn session_keyspace(active: &ActiveKeyspace) -> Option<String> {
    active.read().expect("a lock the test owns").clone()
}

/// A service whose session runs in `app` and that has read one table's key.
///
/// A bare table name then resolves to `app.<table>`; a statement may also name
/// `app.<table>` itself.
fn service_knowing(table: &str, key_columns: &[&str]) -> CqlLanguageService {
    let (service, _active) = service_in_session(Some("app"));
    service.remember_key_columns(
        "app",
        table,
        key_columns.iter().map(|name| name.to_string()).collect(),
    );
    service
}

/// The class and the danger one buffer carries under `service`.
fn class_and_kind(
    service: &CqlLanguageService,
    query: &str,
) -> (Option<ExecutionClassification>, Option<DangerousQueryKind>) {
    (
        service.classify_execution(query),
        service.detect_dangerous(query),
    )
}

#[test]
fn a_cql_select_is_a_read() {
    assert_eq!(
        classify("SELECT * FROM users WHERE id = 1"),
        ExecutionClassification::Read
    );
    assert_eq!(
        classify("select * from users"),
        ExecutionClassification::Read,
        "a bare table name belongs to the current keyspace, not to a system one"
    );
}

#[test]
fn a_select_of_a_system_keyspace_is_metadata() {
    for query in [
        "SELECT release_version FROM system.local",
        "SELECT keyspace_name FROM system_schema.keyspaces",
        "SELECT * FROM system_auth.roles",
        "SELECT * FROM system_distributed_everywhere.view_build_status",
        "SELECT * FROM system_traces.sessions",
        "SELECT * FROM system_views.clients",
        "SELECT * FROM system_virtual_schema.tables",
        "DESCRIBE TABLES",
        "USE users",
    ] {
        assert_eq!(
            classify(query),
            ExecutionClassification::Metadata,
            "{query}"
        );
    }
}

#[test]
fn a_user_keyspace_that_starts_with_system_is_still_a_read() {
    // Ruling 4: the system keyspaces are an explicit list, so a user keyspace
    // whose name merely starts with `system` is user data, not the catalog.
    for query in [
        "SELECT * FROM systems.users",
        "SELECT * FROM system_custom.settings",
        "SELECT * FROM systematic.logs",
    ] {
        assert_eq!(classify(query), ExecutionClassification::Read, "{query}");
    }
}

#[test]
fn cql_writes_and_schema_changes_carry_their_class() {
    assert_eq!(
        classify("INSERT INTO users (id, name) VALUES (1, 'a')"),
        ExecutionClassification::Write
    );
    assert_eq!(
        classify("UPDATE users SET name = 'a' WHERE id = 1"),
        ExecutionClassification::Write
    );
    assert_eq!(
        classify("CREATE TABLE users (id int PRIMARY KEY, name text)"),
        ExecutionClassification::AdminSafe
    );
    assert_eq!(
        classify("ALTER TABLE users ADD email text"),
        ExecutionClassification::Admin
    );
    assert_eq!(
        classify("DROP INDEX users_name_idx"),
        ExecutionClassification::Admin,
        "dropping an index removes no data"
    );
    assert_eq!(
        classify("DROP TABLE users"),
        ExecutionClassification::AdminDestructive
    );
    assert_eq!(
        classify("DROP KEYSPACE app"),
        ExecutionClassification::AdminDestructive
    );
    assert_eq!(
        classify("TRUNCATE users"),
        ExecutionClassification::AdminDestructive
    );
}

#[test]
fn cql_grant_and_revoke_are_administration() {
    // Ruling 3 on #472: core rates these `Admin` for SQL, so the CQL service
    // must not leave them at the `Write` fallback, below `CREATE TABLE`.
    assert_eq!(
        classify("GRANT SELECT ON users TO app"),
        ExecutionClassification::Admin
    );
    assert_eq!(
        classify("REVOKE SELECT ON users FROM app"),
        ExecutionClassification::Admin
    );
    assert_eq!(
        classify("LIST ROLES"),
        ExecutionClassification::Write,
        "a keyword the service does not classify keeps the conservative default"
    );
}

#[test]
fn a_delete_by_the_whole_key_is_a_write_and_confirms_nothing() {
    // A `DELETE` is safe in one shape only: its `WHERE` pins every key column
    // with `=`, which addresses one row. The table's key is what a schema read
    // put in the cache.
    let service = service_knowing("events", &["tenant", "at"]);

    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM app.events WHERE tenant = 'acme' AND at = 1"
        ),
        (Some(ExecutionClassification::Write), None)
    );
    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM app.events WHERE at = 1 AND tenant = 'acme'"
        ),
        (Some(ExecutionClassification::Write), None),
        "the keys may come in any order in the text"
    );
}

#[test]
fn a_delete_of_only_the_partition_key_removes_a_partition() {
    let service = service_knowing("events", &["tenant", "at"]);

    assert_eq!(
        class_and_kind(&service, "DELETE FROM app.events WHERE tenant = 'acme'"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "a missing clustering column leaves the whole partition in range"
    );
}

#[test]
fn a_range_on_the_last_clustering_column_is_partial() {
    let service = service_knowing("events", &["tenant", "at"]);

    for relation in ["at > 1", "at >= 1", "at < 1", "at <= 1"] {
        let query = format!("DELETE FROM app.events WHERE tenant = 'acme' AND {relation}");
        assert_eq!(
            class_and_kind(&service, &query),
            (
                Some(ExecutionClassification::Destructive),
                Some(DangerousQueryKind::CqlPartialKeyDelete)
            ),
            "{query}"
        );
    }
}

#[test]
fn an_in_on_the_partition_key_is_partial() {
    let service = service_knowing("events", &["tenant", "at"]);

    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM app.events WHERE tenant IN ('acme', 'other') AND at = 1"
        ),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "`IN` on a key column selects several keys, so it is no single row"
    );
}

#[test]
fn a_delete_without_a_where_is_partial() {
    let service = service_knowing("events", &["tenant", "at"]);

    assert_eq!(
        class_and_kind(&service, "DELETE FROM app.events"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "CQL rejects this statement; the service flags it rather than trust it"
    );
}

#[test]
fn a_delete_on_a_table_whose_key_was_never_read_is_flagged() {
    // A cold cache is cheaper than a silently removed partition, so a table
    // the service has no key for is never called keyed.
    assert_eq!(
        class_and_kind(
            &fresh(),
            "DELETE FROM app.events WHERE tenant = 'acme' AND at = 1"
        ),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        )
    );

    let known = service_knowing("events", &["tenant", "at"]);
    assert_eq!(
        class_and_kind(&known, "DELETE FROM app.audit WHERE id = 1"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "a table beside a known one is still unknown while its own details are unread"
    );
}

#[test]
fn an_unquoted_identifier_folds_to_lower_case() {
    let service = service_knowing("events", &["tenant", "at"]);

    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM APP.EVENTS WHERE TENANT = 'acme' AND AT = 1"
        ),
        (Some(ExecutionClassification::Write), None),
        "an unquoted name is the lower-case name, whatever case it is typed in"
    );
}

#[test]
fn a_quoted_identifier_keeps_its_case() {
    // `system_schema` spells a mixed-case table and its columns in the case
    // they were created with, so a quoted name matches and an unquoted one
    // folds to lower case and names no such table.
    let service = service_knowing("Events", &["Tenant", "At"]);

    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM app.\"Events\" WHERE \"Tenant\" = 'acme' AND \"At\" = 1"
        ),
        (Some(ExecutionClassification::Write), None)
    );
    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM app.Events WHERE Tenant = 'acme' AND At = 1"
        ),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "an unquoted `Events` is `events`, which this cache does not know"
    );
}

#[test]
fn a_delete_that_also_names_a_non_key_column_is_still_the_whole_key() {
    let service = service_knowing("events", &["tenant", "at"]);

    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM app.events WHERE tenant = 'acme' AND at = 1 AND kind = 'click'"
        ),
        (Some(ExecutionClassification::Write), None),
        "an extra comparison narrows the rows; it does not widen the key"
    );
}

#[test]
fn a_bare_table_name_resolves_against_the_active_keyspace() {
    // The review's data-loss path: `app.users` and `other.users` key
    // differently, and the session runs in `other`. A bare name must be judged
    // by the key of the keyspace the statement will run in, never by another
    // keyspace's key.
    let (service, active) = service_in_session(Some("other"));
    service.remember_key_columns("app", "users", vec!["id".into()]);
    service.remember_key_columns("other", "users", vec!["tenant".into(), "at".into()]);

    assert_eq!(
        class_and_kind(&service, "DELETE FROM users WHERE id = 1"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "the session's `users` is keyed by tenant and at, so `id` alone is partial"
    );

    set_session_keyspace(&active, Some("app"));

    assert_eq!(
        class_and_kind(&service, "DELETE FROM users WHERE id = 1"),
        (Some(ExecutionClassification::Write), None),
        "the same statement against the other keyspace names its whole key there"
    );
}

#[test]
fn a_bare_table_name_in_a_keyspace_nobody_read_is_flagged() {
    // Only `app.users` was read and the session runs in `other`: the gap the
    // review found, where the unread keyspace was invisible and the delete
    // passed on another keyspace's key.
    let (service, _active) = service_in_session(Some("other"));
    service.remember_key_columns("app", "users", vec!["id".into()]);

    assert_eq!(
        class_and_kind(&service, "DELETE FROM users WHERE id = 1"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        )
    );
}

#[test]
fn a_bare_table_name_with_no_session_keyspace_is_flagged() {
    let (service, _active) = service_in_session(None);
    service.remember_key_columns("app", "users", vec!["id".into()]);

    assert_eq!(
        class_and_kind(&service, "DELETE FROM users WHERE id = 1"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "a session with no keyspace cannot say which `users` the statement means"
    );
    assert_eq!(
        class_and_kind(&service, "DELETE FROM app.users WHERE id = 1"),
        (Some(ExecutionClassification::Write), None),
        "a qualified name needs no session keyspace"
    );
}

#[test]
fn a_use_that_ran_moves_the_keyspace_a_bare_name_resolves_in() {
    let (service, active) = service_in_session(Some("app"));
    service.remember_key_columns("app", "users", vec!["id".into()]);
    service.remember_key_columns("staging", "users", vec!["tenant".into(), "at".into()]);

    assert_eq!(
        class_and_kind(&service, "DELETE FROM users WHERE id = 1"),
        (Some(ExecutionClassification::Write), None)
    );

    service.statement_ran("USE staging");

    assert_eq!(
        session_keyspace(&active),
        Some("staging".to_string()),
        "the connection's own keyspace field moves with the statement"
    );
    assert_eq!(
        class_and_kind(&service, "DELETE FROM users WHERE id = 1"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "the session now runs in `staging`, whose key is not `id` alone"
    );
}

#[test]
fn a_statement_that_is_not_a_use_moves_no_keyspace() {
    let (service, active) = service_in_session(Some("app"));
    service.remember_key_columns("app", "users", vec!["id".into()]);

    service.statement_ran("SELECT 'USE staging' FROM app.users");
    service.statement_ran("SELECT * FROM app.users WHERE id = 1");

    assert_eq!(session_keyspace(&active), Some("app".to_string()));
    assert_eq!(
        class_and_kind(&service, "DELETE FROM users WHERE id = 1"),
        (Some(ExecutionClassification::Write), None)
    );
}

#[test]
fn an_admin_statement_forgets_every_cached_key() {
    // A stale entry is worse than a cold one: a table dropped and recreated
    // with a different key would otherwise be judged by the old key, and a
    // partial delete would pass as a full-key one.
    for statement in [
        "CREATE TABLE app.events (id int PRIMARY KEY)",
        "GRANT SELECT ON app.events TO app",
        "DROP TABLE app.events",
        "TRUNCATE app.events",
    ] {
        let (service, _active) = service_in_session(Some("app"));
        service.remember_key_columns("app", "events", vec!["tenant".into(), "at".into()]);
        let delete = "DELETE FROM app.events WHERE tenant = 'acme' AND at = 1";
        assert_eq!(
            class_and_kind(&service, delete),
            (Some(ExecutionClassification::Write), None),
            "{statement}"
        );

        service.statement_ran(statement);

        assert_eq!(
            class_and_kind(&service, delete),
            (
                Some(ExecutionClassification::Destructive),
                Some(DangerousQueryKind::CqlPartialKeyDelete)
            ),
            "{statement} may have rewritten the key"
        );
    }
}

#[test]
fn a_failed_schema_change_forgets_the_keys_and_moves_no_keyspace() {
    // The server can apply a DDL and still answer with an error, so a failure
    // clears the keys the way a success does. A failed `USE` moved nothing,
    // and a failed write changed no key.
    let (service, active) = service_in_session(Some("app"));
    service.remember_key_columns("app", "events", vec!["tenant".into(), "at".into()]);
    let delete = "DELETE FROM app.events WHERE tenant = 'acme' AND at = 1";
    assert_eq!(
        class_and_kind(&service, delete),
        (Some(ExecutionClassification::Write), None)
    );

    service.statement_failed("DROP TABLE app.events");

    assert_eq!(
        class_and_kind(&service, delete),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        ),
        "a DDL that failed may still have been applied"
    );
    assert_eq!(
        session_keyspace(&active),
        Some("app".to_string()),
        "a failed statement moves no keyspace"
    );

    service.remember_key_columns("app", "events", vec!["tenant".into(), "at".into()]);
    service.statement_failed("USE staging");
    service.statement_failed("INSERT INTO app.events (tenant, at) VALUES ('acme', 1)");

    assert_eq!(session_keyspace(&active), Some("app".to_string()));
    assert_eq!(
        class_and_kind(&service, delete),
        (Some(ExecutionClassification::Write), None),
        "a failed `USE` and a failed write leave the keys and the session alone"
    );
}

#[test]
fn a_statement_that_changes_no_schema_keeps_the_keys() {
    let (service, _active) = service_in_session(Some("app"));
    service.remember_key_columns("app", "events", vec!["tenant".into(), "at".into()]);
    let delete = "DELETE FROM app.events WHERE tenant = 'acme' AND at = 1";

    for statement in [
        "INSERT INTO app.events (tenant, at) VALUES ('acme', 1)",
        "UPDATE app.events SET payload = 'x' WHERE tenant = 'acme' AND at = 1",
        "SELECT * FROM app.events",
        delete,
    ] {
        service.statement_ran(statement);

        assert_eq!(
            class_and_kind(&service, delete),
            (Some(ExecutionClassification::Write), None),
            "{statement}"
        );
    }
}

#[test]
fn a_table_details_read_teaches_the_classifier_its_key() {
    // The driver records the key from the `system_schema.columns` rows a
    // details read has already fetched, in the same parse that builds the
    // tree's columns. This is that record, with no server.
    let rows = vec![
        column_row("events", "payload", "regular", -1, "text", "none"),
        column_row("events", "at", "clustering", 0, "timestamp", "asc"),
        column_row("events", "tenant", "partition_key", 0, "text", "none"),
    ];

    let layout = crate::schema::columns_from_rows(&rows).expect("a readable catalog row");
    assert_eq!(
        layout.keys,
        vec!["tenant".to_string(), "at".to_string()],
        "partition key first, then the clustering columns, whatever order the rows came in"
    );

    let (service, _active) = service_in_session(Some("app"));
    service.remember_key_columns("app", "events", layout.keys);

    assert_eq!(
        class_and_kind(
            &service,
            "DELETE FROM app.events WHERE tenant = 'acme' AND at = 1"
        ),
        (Some(ExecutionClassification::Write), None)
    );
    assert_eq!(
        class_and_kind(&service, "DELETE FROM app.events WHERE tenant = 'acme'"),
        (
            Some(ExecutionClassification::Destructive),
            Some(DangerousQueryKind::CqlPartialKeyDelete)
        )
    );
}

#[test]
fn the_partial_key_kind_never_fires_for_another_language() {
    // The kind lives in this driver's service: core reads SQL with its own
    // service, and a Mongo or Redis connection is handed its own driver's, so
    // no `DELETE` under those languages reaches the CQL rule.
    let delete = "DELETE FROM app.events WHERE tenant = 'acme'";

    assert_eq!(
        datatab_core::classify_query_for_language_with_service(
            &QueryLanguage::Sql,
            delete,
            Some(&service_knowing("events", &["tenant", "at"])),
        ),
        ExecutionClassification::Write,
        "SQL reads its own heuristic whatever service a caller hands it"
    );
    assert_ne!(
        datatab_core::SqlLanguageService.detect_dangerous(delete),
        Some(DangerousQueryKind::CqlPartialKeyDelete)
    );

    for language in [QueryLanguage::MongoQuery, QueryLanguage::RedisCommands] {
        assert_ne!(
            datatab_core::classify_query_for_language_with_service(&language, delete, None),
            ExecutionClassification::Destructive,
            "{language:?} keeps its own classification"
        );
    }
}

#[test]
fn a_cql_batch_is_a_write() {
    // The batch splits at its inner `;`, so every statement is classified and
    // the buffer stays at least a write.
    assert_eq!(
        classify(
            "BEGIN BATCH
               INSERT INTO users (id) VALUES (1);
               UPDATE users SET name = 'a' WHERE id = 1;
             APPLY BATCH;"
        ),
        ExecutionClassification::Write
    );

    // A delete whose key the service knows adds nothing to the batch's class.
    let service = service_knowing("users", &["id"]);
    assert_eq!(
        service.classify_execution(
            "BEGIN BATCH
               INSERT INTO users (id) VALUES (1);
               DELETE FROM users WHERE id = 1;
             APPLY BATCH;"
        ),
        Some(ExecutionClassification::Write)
    );
    assert_eq!(
        service.detect_dangerous(
            "BEGIN BATCH
               INSERT INTO users (id) VALUES (1);
               DELETE FROM users WHERE id = 1;
             APPLY BATCH;"
        ),
        None
    );
}

#[test]
fn catalog_reads_hidden_in_a_buffer_are_still_writes() {
    assert_eq!(
        service_knowing("users", &["id"])
            .classify_execution("SELECT * FROM system.local; DELETE FROM users WHERE id = 1"),
        Some(ExecutionClassification::Write),
        "a buffer of several statements is never metadata"
    );
    assert_eq!(
        classify("SELECT * FROM system.local; SELECT keyspace_name FROM system_schema.keyspaces"),
        ExecutionClassification::Write,
        "several statements are never a metadata-only buffer"
    );
}

#[test]
fn a_buffer_that_drops_an_object_and_creates_one_keeps_the_worse_class() {
    // A guard on the CQL fold (#483): a buffer is never less restricted than
    // any of its statements, so `AdminDestructive` outranks the `AdminSafe`
    // create beside it and an `Admin` change beats a create.
    assert_eq!(
        classify("DROP TABLE users; CREATE TABLE users_new (id int PRIMARY KEY)"),
        ExecutionClassification::AdminDestructive
    );
    assert_eq!(
        classify("ALTER TABLE users ADD email text; CREATE TABLE audit (id int PRIMARY KEY)"),
        ExecutionClassification::Admin
    );
}

#[test]
fn a_partial_key_delete_beside_a_create_is_admin_destructive() {
    // The composition guard from #483, now that CQL can produce a
    // `Destructive`: a buffer folds by `compose`, so a delete that removes a
    // range of rows beside a create is the top of both ladders, never the
    // create's own class.
    assert_eq!(
        classify("DELETE FROM users WHERE id = 1; CREATE TABLE audit (id int PRIMARY KEY)"),
        ExecutionClassification::AdminDestructive,
        "the delete's table key is unread here, so the delete is destructive"
    );

    // With the key known, the delete is a write and the buffer is a create's
    // class; `Write` and `AdminSafe` are comparable on both ladders.
    assert_eq!(
        service_knowing("users", &["id"]).classify_execution(
            "DELETE FROM users WHERE id = 1; CREATE TABLE audit (id int PRIMARY KEY)"
        ),
        Some(ExecutionClassification::AdminSafe)
    );
}

#[test]
fn dangerous_statements_report_their_kind() {
    assert_eq!(
        fresh().detect_dangerous("TRUNCATE users"),
        Some(DangerousQueryKind::Truncate)
    );
    assert_eq!(
        fresh().detect_dangerous("DROP TABLE users"),
        Some(DangerousQueryKind::Drop)
    );
    assert_eq!(
        fresh().detect_dangerous("ALTER TABLE users ADD email text"),
        Some(DangerousQueryKind::Alter)
    );
    assert_eq!(fresh().detect_dangerous("SELECT * FROM users"), None);
    assert_eq!(
        fresh().detect_dangerous("INSERT INTO users (id) VALUES (1)"),
        None
    );
    assert_eq!(
        service_knowing("users", &["id"]).detect_dangerous("DELETE FROM app.users WHERE id = 1"),
        None,
        "a delete by its whole key is a write, not a danger"
    );
    assert_eq!(
        fresh().detect_dangerous("DELETE FROM app.users WHERE id = 1"),
        Some(DangerousQueryKind::CqlPartialKeyDelete),
        "a delete nobody has read the key for is flagged"
    );
}

#[test]
fn a_buffer_with_a_dangerous_statement_confirms_once_as_a_script() {
    assert_eq!(
        fresh().detect_dangerous("SELECT * FROM users WHERE id = 1; DROP TABLE users"),
        Some(DangerousQueryKind::Script)
    );
}

#[test]
fn a_batch_of_writes_confirms_nothing() {
    assert_eq!(
        service_knowing("users", &["id"]).detect_dangerous(
            "BEGIN BATCH
               INSERT INTO users (id) VALUES (1);
               DELETE FROM users WHERE id = 1;
             APPLY BATCH;"
        ),
        None
    );
    assert_eq!(
        service_knowing("users", &["id"])
            .detect_dangerous("BEGIN BATCH INSERT INTO users (id) VALUES (1); APPLY BATCH;"),
        None
    );
}

#[test]
fn danger_detection_ignores_keywords_in_literals_and_comments() {
    assert_eq!(
        service_knowing("users", &["name"])
            .detect_dangerous("DELETE FROM app.users WHERE name = 'DROP TABLE t'"),
        None,
        "a literal is not a statement, and the delete names its whole key"
    );
    assert_eq!(
        fresh().detect_dangerous("-- DROP TABLE users\nSELECT * FROM users"),
        None
    );
    assert_eq!(
        fresh().detect_dangerous("/* TRUNCATE users */ SELECT * FROM users"),
        None
    );
}

#[test]
fn validate_reports_an_unbalanced_batch() {
    assert!(matches!(
        fresh().validate("SELECT * FROM users"),
        ValidationResult::Valid
    ));
    assert!(matches!(fresh().validate("   "), ValidationResult::Valid));
    assert!(matches!(
        fresh().validate("BEGIN BATCH INSERT INTO users (id) VALUES (1); APPLY BATCH;"),
        ValidationResult::Valid
    ));
    assert!(matches!(
        fresh().validate("BEGIN BATCH INSERT INTO users (id) VALUES (1);"),
        ValidationResult::SyntaxError(_)
    ));
    assert!(
        matches!(
            fresh().validate("APPLY BATCH;"),
            ValidationResult::SyntaxError(_)
        ),
        "APPLY BATCH alone closes nothing"
    );
    assert!(
        matches!(
            fresh().validate("BEGIN BATCH APPLY BATCH;"),
            ValidationResult::Valid
        ),
        "a batch written without an inner `;` closes the batch it opened"
    );
}

#[test]
fn only_a_trailing_apply_batch_closes_a_batch() {
    // The two words can name columns without closing anything, so the pair
    // must be a statement's last two tokens.
    assert!(matches!(
        fresh().validate("SELECT apply, batch FROM t"),
        ValidationResult::Valid
    ));
    assert!(
        matches!(
            fresh().validate("UPDATE t SET x = 1 WHERE apply = batch"),
            ValidationResult::Valid
        ),
        "a WHERE clause naming both words closes no batch"
    );
    assert!(
        matches!(
            fresh().validate("BEGIN BATCH INSERT INTO t (k) VALUES (1); APPLY BATCH;"),
            ValidationResult::Valid
        ),
        "a real APPLY BATCH still closes its batch"
    );
    assert!(
        matches!(
            fresh().validate("BEGIN BATCH INSERT INTO t (k) VALUES (1);"),
            ValidationResult::SyntaxError(_)
        ),
        "an unclosed batch still reports"
    );
}

#[test]
fn the_cql_service_classifies_a_select_through_the_core_dispatcher() {
    // The service is what `classify_query_for_language_with_service` consults
    // for `QueryLanguage::Cql`; this is the driver's language, handed to the
    // core dispatcher, without a live connection.
    let service = CqlLanguageService::new(ActiveKeyspace::default());
    assert_eq!(
        datatab_core::classify_query_for_language_with_service(
            &METADATA.query_language,
            "SELECT * FROM users",
            Some(&service),
        ),
        ExecutionClassification::Read
    );
}

/// Ruling 6 (issue #472): one rule set per language. The splitter's CQL rules
/// and the service's own lexer must agree on where the statements of a buffer
/// begin and end, or a buffer would be classified as one statement's worth of
/// danger while it runs as several (or the reverse).
///
/// The service splits at every `;` on purpose — a batch's statements are
/// classified one by one — while the splitter merges a batch into the one
/// statement it runs as, so the service counts exactly the inner `;` of each
/// batch more than the splitter does.
///
/// The batch's statements are counted by the splitter itself: its text after
/// the `BEGIN [UNLOGGED|COUNTER] BATCH` header, split again. Counting the `;`
/// characters instead would count the ones inside a literal.
fn batch_body_statement_count(statement: &str) -> usize {
    // CQL keywords are case-insensitive, so the header is found in an
    // uppercase copy; `to_ascii_uppercase` keeps `statement`'s byte offsets,
    // and the body is cut from `statement` to keep the buffer as written.
    let uppercase = statement.to_ascii_uppercase();
    let Some(header) = uppercase.find("BATCH") else {
        return 1;
    };
    let body = statement.get(header + "BATCH".len()..).unwrap_or_default();

    QueryLanguage::Cql.split_statements(body).len()
}

#[test]
fn the_splitter_and_the_service_agree_on_the_statement_count() {
    let buffers = [
        // A `;` inside a string.
        "SELECT * FROM t WHERE name = 'a;b'; SELECT * FROM u",
        // `BEGIN BATCH` inside a line comment.
        "-- BEGIN BATCH INSERT INTO t (k) VALUES (1)\nSELECT * FROM t; SELECT * FROM u",
        // `BEGIN BATCH` inside a string.
        "INSERT INTO t (k) VALUES ('BEGIN BATCH'); SELECT * FROM u",
        // An unclosed batch.
        "BEGIN BATCH INSERT INTO t (k) VALUES (1); DELETE FROM t WHERE k = 1;",
        // A batch between two other statements.
        "SELECT 1; BEGIN BATCH INSERT INTO t (k) VALUES (1); APPLY BATCH; SELECT 2",
        // A `;` inside a `//` comment.
        "SELECT 1; // a; b\nSELECT 2",
        // A string ending in a backslash.
        "SELECT * FROM t WHERE path = 'C:\\'; SELECT 2",
        // A `;` inside a `$$` quoted string.
        "CREATE ROLE app WITH PASSWORD = $$a;b$$; GRANT SELECT ON ALL KEYSPACES TO app",
        // A `;` inside a string that a batch holds.
        "BEGIN BATCH INSERT INTO t (k) VALUES ('a;b'); APPLY BATCH;",
        // A block comment that would nest under SQL's rules.
        "/* a /* b */ SELECT 1; SELECT 2",
        // A `$tag$` body, which is no quote in CQL.
        "SELECT $tag$x;y$tag$; SELECT 2",
        // A batch whose keywords are lowercase.
        "begin batch insert into t (k) values (1); apply batch;",
        // A batch with no statement inside it.
        "BEGIN BATCH APPLY BATCH;",
    ];

    for buffer in buffers {
        let statements = QueryLanguage::Cql.split_statements(buffer);
        let service_count = cql_statement_count(buffer);
        // The service splits at every `;`, a batch's inner ones included, while
        // the splitter merges a batch into the statement it runs as. Deriving
        // the expected count from the splitter's own rules, rather than from
        // the `;` characters in the text, keeps a `;` inside a literal or a
        // comment out of the count.
        let expected_service_count: usize = statements
            .iter()
            .map(|statement| {
                if !statement.to_ascii_uppercase().starts_with("BEGIN") {
                    return 1;
                }

                batch_body_statement_count(statement)
            })
            .sum();

        assert_eq!(
            service_count, expected_service_count,
            "the splitter and the service must agree on the statement count in: {buffer}"
        );

        if statements.len() > 1 {
            let classification = fresh()
                .classify_execution(buffer)
                .expect("the CQL service classifies every buffer");
            assert!(
                !matches!(
                    classification,
                    ExecutionClassification::Metadata | ExecutionClassification::Read
                ),
                "a buffer of {} statements is not a single read or metadata call: {buffer}",
                statements.len()
            );
            assert!(
                !matches!(
                    fresh().detect_dangerous(buffer),
                    Some(
                        DangerousQueryKind::Drop
                            | DangerousQueryKind::Truncate
                            | DangerousQueryKind::Alter
                    )
                ),
                "a buffer of {} statements must not report one statement's danger: {buffer}",
                statements.len()
            );
        }
    }
}

// =============================================================================
// Paging a table browse (part 4, design §4, ruling 1 on #64)
// =============================================================================

#[test]
fn a_browse_sends_no_limit_and_no_offset() {
    let request = TableBrowseRequest::new(TableRef::with_schema("app", "users"))
        .with_pagination(Pagination::Offset {
            limit: 100,
            offset: 300,
        })
        .with_filter("city = 'Berlin'");

    let query = browse_cql(&request).expect("the browse text builds");

    assert_eq!(
        query.cql,
        "SELECT * FROM \"app\".\"users\" WHERE city = 'Berlin'"
    );
    assert_eq!(
        query.filter.as_deref(),
        Some("city = 'Berlin'"),
        "the cache keys on the filter the statement carries"
    );
    assert!(
        !query.cql.contains("LIMIT"),
        "a CQL LIMIT bounds the whole query, so page 2 would be empty"
    );
    assert!(!query.cql.contains("OFFSET"), "CQL has no OFFSET");
}

#[test]
fn a_browse_does_not_order_across_a_partition() {
    let request = TableBrowseRequest::new(TableRef::with_schema("app", "users"))
        .with_order_by(vec![OrderByColumn::asc("name")]);

    let query = browse_cql(&request).expect("the browse text builds");

    assert!(
        !query.cql.contains("ORDER BY"),
        "CQL orders a partition by its clustering columns only"
    );
}

#[test]
fn a_browse_without_a_filter_sends_no_where_clause() {
    let request = TableBrowseRequest::new(TableRef::new("users")).with_filter("   ");

    let query = browse_cql(&request).expect("the browse text builds");

    assert_eq!(query.cql, "SELECT * FROM \"users\"");
    assert!(
        !query.cql.contains("WHERE"),
        "an empty filter adds no clause"
    );
    assert_eq!(query.filter, None, "an empty filter keys nothing");
}

#[test]
fn a_page_number_is_the_offset_over_the_page_size() {
    assert_eq!(
        browse_page_number(100, 0).expect("page 1"),
        (0, 100),
        "the first page starts the scan"
    );
    assert_eq!(
        browse_page_number(100, 300).expect("page 4"),
        (3, 100),
        "the fourth page is page index 3"
    );
    assert!(matches!(
        browse_page_number(100, 150),
        Err(DbError::NotSupported(_))
    ));
    assert!(matches!(
        browse_page_number(0, 0),
        Err(DbError::NotSupported(_))
    ));
}

#[test]
fn a_row_count_is_refused_rather_than_scanned() {
    let refusal = row_count_refusal();
    assert!(matches!(refusal, DbError::NotSupported(_)));
    assert!(
        refusal.to_string().contains("scanning"),
        "the refusal says why: {refusal}"
    );
}

/// A server that answers a page ask with the rows from a cursor kept in the
/// paging state, like a cluster walking a table.
struct FakeServer {
    total_rows: u64,
    /// Rows one server page carries whatever size was asked for, which is how
    /// a real server ends a page short.
    max_per_page: u64,
    asks: Vec<(i32, u64)>,
}

impl FakeServer {
    fn new(total_rows: u64, max_per_page: u64) -> Self {
        Self {
            total_rows,
            max_per_page,
            asks: Vec::new(),
        }
    }

    fn fetch(
        &mut self,
        page_size: i32,
        state: PagingState,
    ) -> Result<(Vec<Vec<Value>>, Option<PagingState>), DbError> {
        let cursor = state
            .as_bytes_slice()
            .and_then(|bytes| bytes.first().copied())
            .map(u64::from)
            .unwrap_or(0);
        self.asks.push((page_size, cursor));

        let wanted = u64::try_from(page_size).unwrap_or(0).min(self.max_per_page);
        let end = (cursor + wanted).min(self.total_rows);
        let rows = (cursor..end)
            .map(|row| vec![Value::Int(row as i64)])
            .collect();
        let next = (end < self.total_rows)
            .then(|| PagingState::new_from_raw_bytes(vec![u8::try_from(end).unwrap_or(0)]));
        Ok((rows, next))
    }
}

fn browse_key(limit: u32) -> BrowseKey {
    BrowseKey::new(
        Some("app".to_string()),
        "users".to_string(),
        None,
        Vec::new(),
        limit,
    )
}

fn row_ids(rows: &[Vec<Value>]) -> Vec<i64> {
    rows.iter()
        .map(|row| match row.first() {
            Some(Value::Int(id)) => *id,
            other => panic!("a row id, got {other:?}"),
        })
        .collect()
}

#[test]
fn page_two_continues_where_page_one_ended() {
    let mut server = FakeServer::new(250, 100);
    let mut cache = PagingCache::default();
    let key = browse_key(100);

    let first = read_grid_page(
        0,
        PagingState::start(),
        0,
        100,
        &mut |size, state| server.fetch(size, state),
        &mut |page, state| cache.insert(&key, page, state),
    )
    .expect("page 1");
    assert_eq!(row_ids(&first.rows), (0..100).collect::<Vec<_>>());

    let next = first.next.expect("the table has more rows");
    cache.insert(&key, 1, next);

    let (start_page, start_state) = cache.start_for(&key, 1).expect("page 1 is kept");
    let second = read_grid_page(
        start_page,
        start_state,
        1,
        100,
        &mut |size, state| server.fetch(size, state),
        &mut |page, state| cache.insert(&key, page, state),
    )
    .expect("page 2");

    assert_eq!(
        row_ids(&second.rows),
        (100..200).collect::<Vec<_>>(),
        "page 2 continues from the state page 1 ended at"
    );
    assert_eq!(server.asks[1], (100, 100), "page 2 asked from row 100");
}

#[test]
fn going_back_to_page_one_reads_from_the_start_again() {
    let mut server = FakeServer::new(250, 100);
    let mut cache = PagingCache::default();
    let key = browse_key(100);

    // The cache keeps no entry for page 0, so a return to it starts the scan.
    assert_eq!(cache.start_for(&key, 0), None);

    let again = read_grid_page(
        0,
        PagingState::start(),
        0,
        100,
        &mut |size, state| server.fetch(size, state),
        &mut |page, state| cache.insert(&key, page, state),
    )
    .expect("page 1 again");

    assert_eq!(row_ids(&again.rows), (0..100).collect::<Vec<_>>());
    assert_eq!(server.asks[0], (100, 0), "the scan restarted at row 0");
}

#[test]
fn a_short_server_page_is_filled_to_the_grid_page() {
    let mut server = FakeServer::new(25, 3);
    let mut cache = PagingCache::default();
    let key = browse_key(10);

    let read = read_grid_page(
        0,
        PagingState::start(),
        0,
        10,
        &mut |size, state| server.fetch(size, state),
        &mut |page, state| cache.insert(&key, page, state),
    )
    .expect("a filled grid page");

    assert_eq!(
        row_ids(&read.rows),
        (0..10).collect::<Vec<_>>(),
        "three short server pages fill one grid page"
    );
    assert_eq!(
        server.asks,
        vec![(10, 0), (7, 3), (4, 6), (1, 9)],
        "each ask is the rows the page still needs"
    );
}

#[test]
fn a_forward_jump_walks_the_pages_in_between_once() {
    let mut server = FakeServer::new(100, 10);
    let mut cache = PagingCache::default();
    let key = browse_key(10);

    let read = read_grid_page(
        0,
        PagingState::start(),
        3,
        10,
        &mut |size, state| server.fetch(size, state),
        &mut |page, state| cache.insert(&key, page, state),
    )
    .expect("page 4");

    assert_eq!(row_ids(&read.rows), (30..40).collect::<Vec<_>>());
    for page in 1..=3 {
        assert!(
            cache.start_for(&key, page).is_some(),
            "page {page} was walked and is kept"
        );
    }
}

#[test]
fn a_jump_past_the_walk_limit_is_refused() {
    let mut server = FakeServer::new(1, 1);
    let outcome = read_grid_page(
        0,
        PagingState::start(),
        MAX_PAGES_WALKED + 1,
        10,
        &mut |size, state| server.fetch(size, state),
        &mut |_, _| {},
    );

    assert!(matches!(outcome, Err(DbError::NotSupported(_))));
}

#[test]
fn the_paging_cache_keeps_a_bounded_number_of_pages() {
    let mut cache = PagingCache::default();
    let key = browse_key(100);

    for page in 1..=MAX_CACHED_PAGES as u64 + 16 {
        cache.insert(
            &key,
            page,
            PagingState::new_from_raw_bytes(vec![u8::try_from(page).unwrap_or(0)]),
        );
    }

    assert_eq!(cache.len(), MAX_CACHED_PAGES);
    assert_eq!(
        cache.start_for(&key, 1),
        None,
        "the oldest page start was evicted"
    );
    assert!(
        cache
            .start_for(&key, MAX_CACHED_PAGES as u64 + 16)
            .is_some(),
        "the newest page start is kept"
    );
}

#[test]
fn another_filter_starts_its_own_walk() {
    let mut cache = PagingCache::default();
    let narrow = BrowseKey::new(
        Some("app".to_string()),
        "users".to_string(),
        Some("city = 'Berlin'".to_string()),
        Vec::new(),
        100,
    );
    cache.insert(&narrow, 1, PagingState::new_from_raw_bytes(vec![9]));

    assert!(
        cache.start_for(&browse_key(100), 1).is_none(),
        "a state belongs to the statement that produced it"
    );
    assert!(cache.start_for(&narrow, 1).is_some());
}

// =============================================================================
// The CQL generator (part 4, design §6)
// =============================================================================

fn select_spec(filter: Option<FilterNode>) -> VisualQuerySpec {
    VisualQuerySpec {
        source: SourceTable {
            schema: Some("app".to_string()),
            table: "users".to_string(),
            alias: "users".to_string(),
        },
        projection: Projection::All,
        joins: vec![],
        filter,
        group_by: vec![],
        aggregates: vec![],
        having: None,
        sort: vec![],
        limit: Some(50),
        offset: 0,
    }
}

fn keyed_filter() -> FilterNode {
    FilterNode::Group {
        op: BoolOp::And,
        children: vec![
            FilterNode::Predicate(Predicate {
                source_alias: "users".to_string(),
                column: "id".to_string(),
                comparator: Comparator::Eq,
                value: PredicateValue::Single(LiteralValue::Integer(7)),
                node_id: 0,
            }),
            FilterNode::Predicate(Predicate {
                source_alias: "users".to_string(),
                column: "city".to_string(),
                comparator: Comparator::In,
                value: PredicateValue::List(vec![
                    LiteralValue::Text("Berlin".to_string()),
                    LiteralValue::Text("Rome".to_string()),
                ]),
                node_id: 0,
            }),
        ],
    }
}

#[test]
fn a_generated_select_binds_the_key_and_limits_the_rows() {
    let query = GENERATOR
        .generate_select(&select_spec(Some(keyed_filter())))
        .expect("a readable spec")
        .expect("CQL text");

    assert_eq!(
        query.sql,
        "SELECT *\nFROM \"app\".\"users\"\nWHERE (\"id\" = ? AND \"city\" IN (?, ?))\nLIMIT 50"
    );
    assert_eq!(
        query.params,
        vec![
            Value::Int(7),
            Value::Text("Berlin".to_string()),
            Value::Text("Rome".to_string())
        ]
    );
}

#[test]
fn a_generated_select_refuses_what_cql_cannot_read() {
    let mut joined = select_spec(None);
    joined.joins = vec![JoinStep {
        kind: JoinKind::Inner,
        from_alias: "users".to_string(),
        to_schema: None,
        to_table: "orders".to_string(),
        to_alias: "orders".to_string(),
        on: JoinOn::FkPath {
            from_column: "id".to_string(),
            to_column: "user_id".to_string(),
        },
    }];
    assert!(matches!(
        GENERATOR.generate_select(&joined),
        Err(QueryGenError::InvalidSpec(_))
    ));

    let mut offset = select_spec(None);
    offset.offset = 100;
    let refusal = GENERATOR
        .generate_select(&offset)
        .expect_err("CQL has no OFFSET");
    assert!(refusal.to_string().contains("OFFSET"));

    let mut sorted = select_spec(None);
    sorted.sort = vec![SortEntry {
        source_alias: "users".to_string(),
        column: "name".to_string(),
        direction: VisualSortDirection::Asc,
    }];
    assert!(matches!(
        GENERATOR.generate_select(&sorted),
        Err(QueryGenError::InvalidSpec(_))
    ));
}

#[test]
fn a_generated_select_renders_an_or_group_as_the_capability_says() {
    let filter = FilterNode::Group {
        op: BoolOp::Or,
        children: vec![
            FilterNode::Predicate(Predicate {
                source_alias: "users".to_string(),
                column: "city".to_string(),
                comparator: Comparator::Eq,
                value: PredicateValue::Single(LiteralValue::Text("Berlin".to_string())),
                node_id: 0,
            }),
            FilterNode::Predicate(Predicate {
                source_alias: "users".to_string(),
                column: "city".to_string(),
                comparator: Comparator::Neq,
                value: PredicateValue::Single(LiteralValue::Text("Rome".to_string())),
                node_id: 0,
            }),
        ],
    };

    let query = GENERATOR
        .generate_select(&select_spec(Some(filter)))
        .expect("an OR group of comparisons")
        .expect("CQL text");

    assert!(
        query.sql.contains("(\"city\" = ? OR \"city\" != ?)"),
        "OR and != reach the statement: {}",
        query.sql
    );
}

#[test]
fn one_identity_helper_judges_what_can_address_a_row() {
    let found = composite_identity(&datatab_core::RowIdentity::new(
        vec!["id".to_string()],
        vec![Value::Int(7)],
    ))
    .expect("one column and one value address a row");
    assert_eq!(found, vec![("id".to_string(), Value::Int(7))]);

    // Built field by field: the constructor asserts the two halves match.
    let mismatched = datatab_core::RowIdentity::Composite {
        columns: vec!["id".to_string(), "bucket".to_string()],
        values: vec![Value::Int(7)],
    };
    assert!(!mismatched.is_valid());
    assert_eq!(
        composite_identity(&mismatched),
        None,
        "the helper uses the identity's own validity rule"
    );
    assert_eq!(
        composite_identity(&datatab_core::RowIdentity::new(Vec::new(), Vec::new())),
        None
    );

    let patch = RowPatch::new(mismatched, "users".to_string(), None, vec![]);
    assert!(
        GENERATOR
            .generate_mutation(&MutationRequest::SqlUpdate(patch))
            .is_none(),
        "the generator refuses the identity the edit path refuses"
    );
}

#[test]
fn a_filter_cql_cannot_express_is_refused() {
    let filter = FilterNode::Predicate(Predicate {
        source_alias: "users".to_string(),
        column: "name".to_string(),
        comparator: Comparator::IsNull,
        value: PredicateValue::None,
        node_id: 0,
    });

    let refusal = GENERATOR
        .generate_select(&select_spec(Some(filter)))
        .expect_err("CQL has no IS NULL in a WHERE clause");
    assert!(matches!(refusal, QueryGenError::InvalidSpec(_)));
}

#[test]
fn a_copied_insert_carries_the_guard_the_grid_uses() {
    let insert = RowInsert::new(
        "users".to_string(),
        Some("app".to_string()),
        vec!["id".to_string(), "name".to_string()],
        vec![Value::Int(7), Value::Text("O'Brien".to_string())],
    );
    let generated = GENERATOR
        .generate_mutation(&MutationRequest::SqlInsert(insert))
        .expect("CQL insert text");

    assert_eq!(generated.language, QueryLanguage::Cql);
    assert_eq!(
        generated.text,
        "INSERT INTO \"app\".\"users\" (\"id\", \"name\") VALUES (7, 'O''Brien') IF NOT EXISTS;"
    );
}

#[test]
fn a_copied_delete_names_the_key_and_carries_if_exists() {
    let delete = RowDelete::new(
        datatab_core::RowIdentity::new(vec!["id".to_string()], vec![Value::Int(7)]),
        "users".to_string(),
        Some("app".to_string()),
    );
    let generated = GENERATOR
        .generate_mutation(&MutationRequest::SqlDelete(delete))
        .expect("CQL delete text");

    assert_eq!(
        generated.text,
        "DELETE FROM \"app\".\"users\" WHERE \"id\" = 7 IF EXISTS;"
    );
}

#[test]
fn a_visual_update_carries_if_exists_and_binds_its_values() {
    let spec = VisualMutationSpec {
        from: TableRef::with_schema("app", "users"),
        filter: Some(keyed_filter()),
        kind: MutationKind::Update {
            assignments: vec![datatab_core::Assignment {
                column: "name".to_string(),
                value: AssignmentValue::Literal(datatab_core::ScalarLiteral::Text(
                    "Ada".to_string(),
                )),
            }],
        },
    };

    let generated = GENERATOR
        .generate_update_from_spec(&spec)
        .expect("a keyed update");

    assert!(
        generated.sql.ends_with("IF EXISTS"),
        "the run refuses a missing row: {}",
        generated.sql
    );
    assert!(
        generated
            .sql
            .starts_with("UPDATE \"app\".\"users\"\nSET \"name\" = ?")
    );
    assert_eq!(
        generated.params.len(),
        4,
        "one value to set and three in the key predicate"
    );
    assert!(!generated.used_raw_expression);
}

#[test]
fn a_visual_delete_without_a_where_is_refused() {
    let spec = VisualMutationSpec {
        from: TableRef::with_schema("app", "users"),
        filter: None,
        kind: MutationKind::Delete,
    };

    let refusal = GENERATOR
        .generate_delete_from_spec(&spec)
        .expect_err("CQL has no bare DELETE");
    assert!(refusal.to_string().contains("WHERE"));
}

#[test]
fn a_read_template_selects_by_the_full_key() {
    let columns = vec![
        datatab_core::ColumnInfo {
            name: "id".to_string(),
            type_name: "int".to_string(),
            nullable: false,
            is_primary_key: true,
            default_value: None,
            enum_values: None,
        },
        datatab_core::ColumnInfo {
            name: "bucket".to_string(),
            type_name: "int".to_string(),
            nullable: false,
            is_primary_key: true,
            default_value: None,
            enum_values: None,
        },
        datatab_core::ColumnInfo {
            name: "name".to_string(),
            type_name: "text".to_string(),
            nullable: true,
            is_primary_key: false,
            default_value: None,
            enum_values: None,
        },
    ];
    let generated = GENERATOR
        .generate_read_template(&ReadTemplateRequest {
            operation: ReadTemplateOperation::SelectWhere,
            schema: Some("app"),
            table: "users",
            columns: &columns,
            options: datatab_core::SqlGenerationOptions::default(),
        })
        .expect("a select template");

    assert_eq!(
        generated.text,
        "SELECT * FROM \"app\".\"users\" WHERE \"id\" = ? AND \"bucket\" = ?;"
    );
}

#[test]
fn the_cql_dialect_writes_cql_literals() {
    assert_eq!(DIALECT.quote_identifier("a\"b"), "\"a\"\"b\"");
    assert_eq!(
        DIALECT.value_to_literal(&Value::Bytes(vec![0xde, 0xad])),
        "0xdead"
    );
    assert_eq!(
        DIALECT.value_to_literal(&Value::Array(vec![Value::Int(1), Value::Int(2)])),
        "[1, 2]"
    );
    assert_eq!(DIALECT.value_to_literal(&Value::Bool(true)), "true");
    assert_eq!(
        DIALECT.select_limit_offset_clause(Some(10), 0, false),
        Some("LIMIT 10".to_string())
    );
    let refused = DIALECT
        .select_limit_offset_clause(Some(10), 5, false)
        .expect("a clause is written");
    assert!(
        refused.contains("OFFSET 5"),
        "a non-zero offset is not dropped silently; the server refuses it: {refused}"
    );
}

// =============================================================================
// Row edits through lightweight transactions (part 4, design §4, ruling 5)
// =============================================================================

#[test]
fn an_update_names_the_full_key_and_never_upserts() {
    let statement = update_statement(
        "app",
        "users",
        &["id".to_string(), "bucket".to_string()],
        &[
            ("bucket".to_string(), Value::Int(2)),
            ("id".to_string(), Value::Int(7)),
        ],
        &[ColumnAssignment::new(
            "name",
            Value::Text("Ada".to_string()),
        )],
    )
    .expect("a keyed update");

    assert_eq!(
        statement.cql,
        "UPDATE \"app\".\"users\" SET \"name\" = ? WHERE \"id\" = ? AND \"bucket\" = ? IF EXISTS"
    );
    assert_eq!(
        statement.values,
        vec![Value::Text("Ada".to_string()), Value::Int(7), Value::Int(2)],
        "the set values come first, then the key in the table's key order"
    );
}

#[test]
fn an_update_without_the_whole_key_is_refused() {
    let refused = update_statement(
        "app",
        "users",
        &["id".to_string(), "bucket".to_string()],
        &[("id".to_string(), Value::Int(7))],
        &[ColumnAssignment::new(
            "name",
            Value::Text("Ada".to_string()),
        )],
    );

    assert!(
        matches!(refused, Err(DbError::NotSupported(_))),
        "a partial key would write a whole partition"
    );
}

#[test]
fn an_identity_with_a_column_outside_the_key_is_refused() {
    let refused = delete_statement(
        "app",
        "users",
        &["id".to_string()],
        &[
            ("id".to_string(), Value::Int(7)),
            ("name".to_string(), Value::Text("Ada".to_string())),
        ],
    );

    assert!(matches!(refused, Err(DbError::NotSupported(_))));
}

#[test]
fn a_delete_names_the_full_key_and_never_silently_misses() {
    let statement = delete_statement(
        "app",
        "users",
        &["id".to_string()],
        &[("id".to_string(), Value::Int(7))],
    )
    .expect("a keyed delete");

    assert_eq!(
        statement.cql,
        "DELETE FROM \"app\".\"users\" WHERE \"id\" = ? IF EXISTS"
    );
    assert_eq!(statement.values, vec![Value::Int(7)]);
}

#[test]
fn an_insert_is_guarded_against_overwriting_a_row() {
    let statement = insert_statement(
        "app",
        "users",
        &["id".to_string()],
        &[
            ColumnAssignment::new("id", Value::Int(7)),
            ColumnAssignment::new("name", Value::Text("Ada".to_string())),
        ],
    )
    .expect("a keyed insert");

    assert_eq!(
        statement.cql,
        "INSERT INTO \"app\".\"users\" (\"id\", \"name\") VALUES (?, ?) IF NOT EXISTS"
    );
    assert_eq!(
        statement.values,
        vec![Value::Int(7), Value::Text("Ada".to_string())]
    );
}

#[test]
fn an_insert_that_leaves_a_key_column_out_is_refused() {
    let refused = insert_statement(
        "app",
        "users",
        &["id".to_string(), "bucket".to_string()],
        &[ColumnAssignment::new("id", Value::Int(7))],
    );

    assert!(matches!(refused, Err(DbError::QueryFailed(_))));
}

fn applied_result(applied: bool) -> QueryResult {
    QueryResult::table(
        vec![datatab_core::ColumnMeta {
            name: "[applied]".to_string(),
            type_name: "boolean".to_string(),
            kind: datatab_core::ColumnKind::Unknown,
            nullable: false,
            is_primary_key: false,
        }],
        vec![vec![Value::Bool(applied)]],
        None,
        std::time::Duration::ZERO,
    )
}

#[test]
fn a_refused_lightweight_transaction_is_an_error() {
    let refusal = not_applied(&applied_result(false), "The row update")
        .expect("a refused transaction is an error");
    assert!(
        refusal.to_string().contains("not applied"),
        "the message says what the server did: {refusal}"
    );

    assert!(
        not_applied(&applied_result(true), "The row update").is_none(),
        "an applied transaction is a success"
    );
    assert!(
        not_applied(&QueryResult::empty(), "The row update").is_none(),
        "a void answer carries no refusal"
    );
}

#[test]
fn a_materialized_view_and_a_counter_table_refuse_row_edits() {
    let view = RelationKind::MaterializedView
        .edit_refusal()
        .expect("a view is read-only");
    assert!(view.to_string().contains("base table"));
    assert!(matches!(view, DbError::NotSupported(_)));

    let counter = RelationKind::CounterTable
        .edit_refusal()
        .expect("a counter table is read-only");
    assert!(counter.to_string().contains("increment"));

    assert!(RelationKind::Table.edit_refusal().is_none());
}

#[test]
fn a_table_with_a_counter_column_is_read_only() {
    let layout = crate::schema::columns_from_rows(&[
        column_row("counters", "key", "partition_key", 0, "text", "none"),
        column_row("counters", "hits", "regular", -1, "counter", "none"),
    ])
    .expect("a counter table's columns");

    assert!(layout.has_counter);
    assert_eq!(layout.keys, vec!["key".to_string()]);
}

#[test]
fn a_table_without_a_counter_column_accepts_edits() {
    let layout = crate::schema::columns_from_rows(&[
        column_row("users", "id", "partition_key", 0, "uuid", "none"),
        column_row("users", "name", "regular", -1, "text", "none"),
    ])
    .expect("a table's columns");

    assert!(!layout.has_counter);
}

// =============================================================================
// Test helpers
// =============================================================================

fn column_row(
    table: &str,
    name: &str,
    kind: &str,
    position: i64,
    type_name: &str,
    clustering_order: &str,
) -> Vec<Value> {
    vec![
        Value::Text(table.to_string()),
        Value::Text(name.to_string()),
        Value::Text(kind.to_string()),
        Value::Int(position),
        Value::Text(type_name.to_string()),
        Value::Text(clustering_order.to_string()),
    ]
}

fn config_with(change: impl FnOnce(&mut DbConfig)) -> DbConfig {
    let mut config = DbConfig::default_cassandra();
    change(&mut config);
    config
}

fn profile_with(config: DbConfig) -> ConnectionProfile {
    ConnectionProfile::new("cassandra-test", config)
}
