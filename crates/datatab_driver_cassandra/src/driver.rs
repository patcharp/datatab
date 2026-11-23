//! Driver registration surface: metadata, form, connect and validation.

use std::sync::LazyLock;

use datatab_core::secrecy::{ExposeSecret, SecretString};
use datatab_core::{
    Connection, ConnectionProfile, DatabaseCategory, DbConfig, DbDriver, DbError, DbKind,
    DeploymentClass, DriverCapabilities, DriverFormDef, DriverLimits, DriverMetadata,
    FormFieldKind, FormSection, FormSectionIcon, FormTab, FormValues, Icon, MutationCapabilities,
    OrderByMode, PaginationStyle, PlaceholderStyle, QueryCapabilities, QueryLanguage,
    SslCertFields, SslModeOption, SyntaxInfo, TransferFamily, WhereOperator, field, field_required,
};

use crate::connection::CassandraConnection;
use crate::session::open_session;
use crate::tls::TlsMode;

/// TLS modes in the words the other drivers use (ruling 7).
pub const SSL_MODES: &[SslModeOption] = &[
    SslModeOption {
        id: "disable",
        label: "disable",
    },
    SslModeOption {
        id: "require",
        label: "require",
    },
    SslModeOption {
        id: "verify-ca",
        label: "verify-ca",
    },
    SslModeOption {
        id: "verify-full",
        label: "verify-full",
    },
];

pub static CASSANDRA_FORM: LazyLock<DriverFormDef> = LazyLock::new(|| DriverFormDef {
    tabs: vec![FormTab {
        id: "main".into(),
        label: "Main".into(),
        sections: vec![FormSection {
            title: "Connection".into(),
            icon: Some(FormSectionIcon::Connection),
            fields: vec![
                field_required(
                    "contact_points",
                    "Contact Points",
                    FormFieldKind::Text,
                    "127.0.0.1",
                ),
                field_required("port", "Port", FormFieldKind::Number, "9042"),
                field("datacenter", "Datacenter", FormFieldKind::Text, "dc1"),
                field("keyspace", "Keyspace", FormFieldKind::Text, ""),
                field("user", "User", FormFieldKind::Text, ""),
                field("password", "Password", FormFieldKind::Password, ""),
            ],
        }],
    }],
});

pub static METADATA: LazyLock<DriverMetadata> = LazyLock::new(|| DriverMetadata {
    id: "cassandra".into(),
    display_name: "Cassandra / ScyllaDB".into(),
    description: "Cassandra and ScyllaDB clusters over CQL".into(),
    category: DatabaseCategory::Relational,
    // CQL is not SQL: the transfer wizard emits SQL DDL, so Cassandra is a
    // target for nothing, as ClickHouse is (design §1).
    transfer_family: TransferFamily::Incompatible,
    deployment_class: Some(DeploymentClass::SelfHosted),
    query_language: QueryLanguage::Cql,
    // What this driver does is what it claims here: CQL, schema browsing, the
    // statement classifier, paging, prepared statements and row edits. Each
    // capability arrived with the part that implemented it, and the live suites
    // in `tests/live_integration.rs` run these against both servers.
    // `SORTING` is never declared: see `query` below.
    capabilities: DriverCapabilities::from_bits_truncate(
        DriverCapabilities::AUTHENTICATION.bits()
            | DriverCapabilities::MULTIPLE_DATABASES.bits()
            | DriverCapabilities::SSL.bits()
            | DriverCapabilities::REQUEST_ROW_LIMIT.bits()
            | DriverCapabilities::VIEWS.bits()
            | DriverCapabilities::INDEXES.bits()
            | DriverCapabilities::CUSTOM_TYPES.bits()
            | DriverCapabilities::PAGINATION.bits()
            | DriverCapabilities::FILTERING.bits()
            | DriverCapabilities::PREPARED_STATEMENTS.bits()
            | DriverCapabilities::INSERT.bits()
            | DriverCapabilities::UPDATE.bits()
            | DriverCapabilities::DELETE.bits(),
    ),
    default_port: Some(9042),
    uri_scheme: "cassandra".into(),
    icon: Icon::Database,
    syntax: Some(SyntaxInfo {
        identifier_quote: '"',
        string_quote: '\'',
        placeholder_style: PlaceholderStyle::QuestionMark,
        // Keyspaces are the CQL schema layer.
        supports_schemas: true,
        default_schema: None,
        // CQL folds unquoted identifiers to lower case.
        case_sensitive_identifiers: false,
        misreads_unknown_quoted_identifiers: false,
    }),
    // CQL has no upsert-on-conflict, no RETURNING and no multi-row VALUES: a
    // row is inserted, updated or deleted one at a time, through a prepared
    // lightweight transaction.
    query: Some(QueryCapabilities {
        // A page is a server paging state, not an offset.
        pagination: vec![PaginationStyle::PageToken],
        // What the generator's `render_filter` can write: CQL reads `=`, `!=`,
        // the range comparisons and `IN`, combined with AND or OR.
        where_operators: vec![
            WhereOperator::Eq,
            WhereOperator::Ne,
            WhereOperator::Gt,
            WhereOperator::Gte,
            WhereOperator::Lt,
            WhereOperator::Lte,
            WhereOperator::In,
            WhereOperator::And,
            WhereOperator::Or,
        ],
        max_query_parameters: 65_535,
        // CQL orders a partition by its clustering columns and nothing else, so
        // the driver offers no sort; the grid sorts a table of this driver in
        // memory instead (ruling 4).
        supports_order_by: false,
        order_by_mode: OrderByMode::None,
        max_order_by_columns: 0,
        supports_group_by: false,
        max_group_by_columns: 0,
        supports_having: false,
        supports_distinct: false,
        supports_limit: true,
        supports_offset: false,
        supports_joins: false,
        supports_subqueries: false,
        supports_union: false,
        supports_intersect: false,
        supports_except: false,
        supports_case_expressions: false,
        supports_window_functions: false,
        supports_ctes: false,
        supports_explain: false,
    }),
    mutation: Some(MutationCapabilities {
        supports_insert: true,
        supports_update: true,
        supports_delete: true,
        // `UPDATE … IF EXISTS` and `INSERT … IF NOT EXISTS` refuse to create
        // or replace a row, which is the opposite of an upsert.
        supports_upsert: false,
        // A CQL write answers with a void result, not the written row.
        supports_returning: false,
        ..Default::default()
    }),
    ddl: None,
    // CQL has no transactions; row edits use lightweight transactions.
    transactions: None,
    limits: Some(DriverLimits {
        max_query_length: 1_000_000,
        max_parameters: 65_535,
        max_result_rows: 0,
        max_connections: 0,
        max_nested_subqueries: 0,
        max_identifier_length: 48,
        max_columns: 0,
        max_indexes_per_table: 0,
        max_bulk_insert_rows: 0,
    }),
    ssl_modes: Some(SSL_MODES),
    // The TLS mode and the certificate paths are the shared Transport control's
    // (see the connection manager's write-back), so the form has no TLS field of
    // its own and there is one source of truth per setting.
    ssl_cert_fields: Some(SslCertFields {
        root_cert: true,
        client_cert: true,
        root_cert_modes: Some(&["verify-ca", "verify-full"]),
        client_cert_modes: Some(&["require", "verify-ca", "verify-full"]),
    }),
    classification_override: None,
    default_chunk_size: None,
    supports_lock_timeout: false,
    editor_profile: None,
});

/// The one driver that serves Cassandra and ScyllaDB.
#[derive(Default)]
pub struct CassandraDriver;

impl CassandraDriver {
    pub fn new() -> Self {
        Self
    }

    fn config(profile: &ConnectionProfile) -> Result<&DbConfig, DbError> {
        match &profile.config {
            config @ DbConfig::Cassandra { .. } => Ok(config),
            _ => Err(DbError::InvalidProfile(
                "Profile is not a Cassandra configuration".into(),
            )),
        }
    }
}

impl DbDriver for CassandraDriver {
    fn kind(&self) -> DbKind {
        DbKind::Cassandra
    }

    fn metadata(&self) -> &DriverMetadata {
        &METADATA
    }

    fn driver_key(&self) -> datatab_core::DriverKey {
        "builtin:cassandra".into()
    }

    fn form_definition(&self) -> &DriverFormDef {
        &CASSANDRA_FORM
    }

    fn secret_field_label(&self, _values: &FormValues) -> Option<String> {
        Some("Password".to_string())
    }

    fn build_config(&self, values: &FormValues) -> Result<DbConfig, DbError> {
        let contact_points = parse_contact_points(values.get("contact_points"))?;
        let port = parse_port(values.get("port"))?;
        let datacenter = values.get("datacenter").cloned().unwrap_or_default();
        let keyspace = values.get("keyspace").cloned().unwrap_or_default();
        let user = values.get("user").cloned().unwrap_or_default();

        Ok(DbConfig::Cassandra {
            contact_points,
            port,
            datacenter: datacenter.trim().to_string(),
            keyspace: keyspace.trim().to_string(),
            user: user.trim().to_string(),
            // The TLS mode and the certificate paths come from the shared
            // Transport control, which writes them into the config after this.
            ssl_mode: Some(TlsMode::Disable.as_id().to_string()),
            ssl_root_cert_path: None,
            ssl_client_cert_path: None,
            ssl_client_key_path: None,
            ssh_tunnel: None,
            ssh_tunnel_profile_id: None,
        })
    }

    fn extract_values(&self, config: &DbConfig) -> FormValues {
        let mut values = FormValues::new();
        let DbConfig::Cassandra {
            contact_points,
            port,
            datacenter,
            keyspace,
            user,
            ..
        } = config
        else {
            return values;
        };

        values.insert("contact_points".to_string(), contact_points.clone());
        values.insert("port".to_string(), port.to_string());
        values.insert("datacenter".to_string(), datacenter.clone());
        values.insert("keyspace".to_string(), keyspace.clone());
        values.insert("user".to_string(), user.clone());
        values
    }

    /// Pure checks only: no I/O and no secret reads (#72).
    ///
    /// The crate has no proxy support, so a proxy access kind is refused here
    /// rather than at connect time. An SSH tunnel is refused too: this part
    /// cannot yet prove the session stays pinned to the one forwarded node,
    /// and an unpinned session would reach the cluster's other nodes
    /// directly, around the tunnel (ruling 6).
    fn validate_profile(&self, profile: &ConnectionProfile) -> Result<(), DbError> {
        let access = profile
            .access_kind
            .clone()
            .unwrap_or_else(|| profile.legacy_access_kind());
        match access {
            datatab_core::AccessKind::Proxy { .. } => {
                return Err(DbError::InvalidProfile(
                    "Cassandra over a proxy is not supported yet".into(),
                ));
            }
            datatab_core::AccessKind::Ssh { .. } => {
                return Err(DbError::InvalidProfile(
                    "Cassandra over an SSH tunnel is not supported in this version".into(),
                ));
            }
            datatab_core::AccessKind::Managed { provider, .. } => {
                return Err(DbError::InvalidProfile(format!(
                    "Cassandra over managed access '{provider}' is not supported yet"
                )));
            }
            datatab_core::AccessKind::Direct => {}
        }

        let config = Self::config(profile)?;
        let DbConfig::Cassandra {
            contact_points,
            port,
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

        parse_contact_points(Some(contact_points))?;
        parse_port(Some(&port.to_string()))?;
        let mode = parse_ssl_mode(ssl_mode.as_ref())?;

        if mode.requires_root_cert() && ssl_root_cert_path.is_none() {
            log::debug!(
                "Cassandra TLS mode {} verifies against the platform roots when no CA file is set",
                mode.as_id()
            );
        }
        // Mutual TLS needs both halves; one without the other cannot be used.
        match (ssl_client_cert_path, ssl_client_key_path) {
            (Some(_), None) => {
                return Err(DbError::InvalidProfile(
                    "A client certificate also needs its private key".into(),
                ));
            }
            (None, Some(_)) => {
                return Err(DbError::InvalidProfile(
                    "A client key also needs its client certificate".into(),
                ));
            }
            _ => {}
        }
        if (ssl_client_cert_path.is_some() || ssl_client_key_path.is_some())
            && mode == TlsMode::Disable
        {
            return Err(DbError::InvalidProfile(
                "A client certificate needs TLS; choose a TLS mode".into(),
            ));
        }

        Ok(())
    }

    fn requires_password(&self) -> bool {
        // Cassandra clusters that allow anonymous access are common in
        // development, so a password is not required to save or connect. The
        // form still declares a password field, which is what draws the secret
        // input for an authenticated cluster.
        false
    }

    fn connect_with_secrets(
        &self,
        profile: &ConnectionProfile,
        password: Option<&SecretString>,
        _ssh_secret: Option<&SecretString>,
    ) -> Result<Box<dyn Connection>, DbError> {
        self.validate_profile(profile)?;
        let config = Self::config(profile)?;

        let password = password.map(|value| value.expose_secret().to_string());
        let (worker, session) = open_session(config, password.as_deref())?;
        Ok(Box::new(CassandraConnection::new(worker, session)))
    }

    fn test_connection(&self, profile: &ConnectionProfile) -> Result<(), DbError> {
        self.validate_profile(profile)?;
        let config = Self::config(profile)?;
        let (worker, session) = open_session(config, None)?;
        let connection = CassandraConnection::new(worker, session);
        let ping = connection.ping();
        let mut connection = connection;
        let close = connection.close();
        ping.and(close)
    }
}

/// Contact points are hostnames or IP addresses, comma-separated. A scheme, a
/// user or a port belongs in another field, so a value that carries one is
/// refused instead of being guessed at.
pub(crate) fn parse_contact_points(value: Option<&String>) -> Result<String, DbError> {
    let raw = value.map(String::as_str).unwrap_or_default().trim();
    if raw.is_empty() {
        return Err(DbError::InvalidProfile(
            "At least one contact point is required".into(),
        ));
    }
    if raw.contains("://") {
        return Err(DbError::InvalidProfile(
            "Contact points take a host name or IP address, not a URL".into(),
        ));
    }

    let mut hosts = Vec::new();
    for host in raw.split(',') {
        let host = host.trim();
        if host.is_empty() {
            continue;
        }
        if host.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '@' | '/' | '?' | '#')
        }) {
            return Err(DbError::InvalidProfile(format!(
                "Contact point '{host}' is not a host name or IP address"
            )));
        }
        if host.contains(':') && host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(DbError::InvalidProfile(format!(
                "Contact point '{host}' carries a port; put the port in the Port field"
            )));
        }
        hosts.push(host);
    }

    if hosts.is_empty() {
        return Err(DbError::InvalidProfile(
            "At least one contact point is required".into(),
        ));
    }
    Ok(hosts.join(","))
}

pub(crate) fn parse_port(value: Option<&String>) -> Result<u16, DbError> {
    let raw = value.map(String::as_str).unwrap_or_default().trim();
    if raw.is_empty() {
        return Ok(9042);
    }
    match raw.parse::<u16>() {
        Ok(0) | Err(_) => Err(DbError::InvalidProfile(format!(
            "Port '{raw}' is not a number between 1 and 65535"
        ))),
        Ok(port) => Ok(port),
    }
}

pub(crate) fn parse_ssl_mode(value: Option<&String>) -> Result<TlsMode, DbError> {
    match value.map(String::as_str).unwrap_or_default().trim() {
        "" => Ok(TlsMode::Disable),
        id => TlsMode::parse(id),
    }
}
