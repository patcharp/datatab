//! Schema browsing from `system_schema`.
//!
//! A keyspace is the database layer and holds no inner schema, so every read is
//! fully qualified and the sidebar loads one keyspace at a time
//! (`SchemaLoadingStrategy::LazyPerDatabase`). The tree is built from names
//! only; columns, key roles and indexes arrive with `table_details`.

use datatab_core::{
    CollectionPresentation, ColumnInfo, Connection, CustomTypeInfo, CustomTypeKind, DatabaseInfo,
    DbError, DbSchemaInfo, IndexData, IndexInfo, SchemaColumnInfo, SchemaIndexInfo, TableInfo,
    TableStorageHint, Value, ViewInfo,
};

use crate::connection::CassandraConnection;
use crate::edit::RelationKind;

// No statement carries an `ORDER BY`, and none relies on one. The driver sorts
// the keyspace list and the columns it reads; every other name keeps the order
// the server sent. Cassandra answers a read that binds a whole partition key in
// that partition's clustering order, and the two bulk reads bind only the
// `keyspace_name` part of their key.
const LIST_KEYSPACES_CQL: &str = "SELECT keyspace_name FROM system_schema.keyspaces";
const LIST_TABLES_CQL: &str = "SELECT table_name FROM system_schema.tables WHERE keyspace_name = ?";
const LIST_VIEWS_CQL: &str = "SELECT view_name FROM system_schema.views WHERE keyspace_name = ?";
const LIST_TYPES_CQL: &str = "SELECT type_name FROM system_schema.types WHERE keyspace_name = ?";
pub(crate) const LIST_COLUMNS_CQL: &str = "SELECT table_name, column_name, kind, position, type, clustering_order \
     FROM system_schema.columns WHERE keyspace_name = ? AND table_name = ?";
const LIST_SCHEMA_COLUMNS_CQL: &str = "SELECT table_name, column_name, kind, position, type, clustering_order \
     FROM system_schema.columns WHERE keyspace_name = ?";
const LIST_INDEXES_CQL: &str = "SELECT index_name, options FROM system_schema.indexes \
     WHERE keyspace_name = ? AND table_name = ?";
const LIST_SCHEMA_INDEXES_CQL: &str =
    "SELECT index_name, table_name, options FROM system_schema.indexes WHERE keyspace_name = ?";
const VIEW_BASE_TABLE_CQL: &str =
    "SELECT base_table_name FROM system_schema.views WHERE keyspace_name = ? AND view_name = ?";

/// The CQL type of a counter column.
const COUNTER_TYPE: &str = "counter";

/// The keyspace and the prefix that name the server's own keyspaces; they are
/// listed after the user's keyspaces.
const SYSTEM_KEYSPACE: &str = "system";
const SYSTEM_KEYSPACE_PREFIX: &str = "system_";

/// Every keyspace on the cluster, the server's own last.
pub(crate) fn list_keyspaces(
    connection: &CassandraConnection,
) -> Result<Vec<DatabaseInfo>, DbError> {
    let result = connection.run_catalog_query(LIST_KEYSPACES_CQL, Vec::new())?;
    let current = connection.active_database();
    let mut names = text_column(&result.rows, 0, "keyspace_name")?;
    sort_keyspaces(&mut names);

    Ok(names
        .into_iter()
        .map(|name| DatabaseInfo {
            is_current: current.as_deref() == Some(name.as_str()),
            name,
        })
        .collect())
}

/// The user's keyspaces first, then the server's, each group alphabetical.
pub(crate) fn sort_keyspaces(names: &mut [String]) {
    names.sort_by(|left, right| {
        is_system_keyspace(left)
            .cmp(&is_system_keyspace(right))
            .then_with(|| left.cmp(right))
    });
}

pub(crate) fn is_system_keyspace(name: &str) -> bool {
    name == SYSTEM_KEYSPACE || name.starts_with(SYSTEM_KEYSPACE_PREFIX)
}

/// The objects of one keyspace: table, view and user-type names only.
///
/// `system_schema.tables` holds tables only. Cassandra keeps a materialized
/// view in `system_schema.views` and in that view's `system_schema.columns`
/// rows; on the tested server version (Cassandra 5.0) it writes no row of
/// `system_schema.tables` for the view, so the two name lists do not overlap.
/// The live test asserts that against a real server, and it is what keeps a
/// view from appearing twice in the tree.
pub(crate) fn schema_for_database(
    connection: &CassandraConnection,
    keyspace: &str,
) -> Result<DbSchemaInfo, DbError> {
    let result =
        connection.run_catalog_query(LIST_TABLES_CQL, vec![Value::Text(keyspace.to_string())])?;
    let tables = text_column(&result.rows, 0, "table_name")?
        .iter()
        .map(|name| shallow_table(keyspace, name))
        .collect();

    let view_rows = connection
        .run_catalog_query(LIST_VIEWS_CQL, vec![Value::Text(keyspace.to_string())])?
        .rows;
    let views: Vec<ViewInfo> = text_column(&view_rows, 0, "view_name")?
        .into_iter()
        .map(|name| ViewInfo {
            name,
            schema: Some(keyspace.to_string()),
        })
        .collect();
    // A materialized view is read-only, and the edit path must know that
    // before it is asked for an edit rather than after the server refuses one.
    for view in &views {
        connection.remember_relation(keyspace, &view.name, RelationKind::MaterializedView);
    }

    Ok(DbSchemaInfo {
        name: keyspace.to_string(),
        tables,
        views,
        custom_types: Some(user_types(connection, keyspace)?),
    })
}

/// The user-defined types of one keyspace.
///
/// A CQL user type is a named composite of fields, which is what
/// [`CustomTypeKind::Composite`] is for; the enum and domain kinds belong to
/// engines that have them.
pub(crate) fn user_types(
    connection: &CassandraConnection,
    keyspace: &str,
) -> Result<Vec<CustomTypeInfo>, DbError> {
    let result =
        connection.run_catalog_query(LIST_TYPES_CQL, vec![Value::Text(keyspace.to_string())])?;
    Ok(text_column(&result.rows, 0, "type_name")?
        .into_iter()
        .map(|name| CustomTypeInfo {
            name,
            schema: Some(keyspace.to_string()),
            kind: CustomTypeKind::Composite,
            enum_values: None,
            base_type: None,
        })
        .collect())
}

/// Every index of one keyspace, in one call.
pub(crate) fn schema_indexes(
    connection: &CassandraConnection,
    keyspace: &str,
) -> Result<Vec<SchemaIndexInfo>, DbError> {
    let result = connection.run_catalog_query(
        LIST_SCHEMA_INDEXES_CQL,
        vec![Value::Text(keyspace.to_string())],
    )?;
    let mut indexes = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        indexes.push(SchemaIndexInfo {
            name: required_text(row, 0, "index_name")?,
            table_name: required_text(row, 1, "table_name")?,
            columns: row.get(2).and_then(index_target).into_iter().collect(),
            // CQL has no unique secondary index and no index-backed primary key
            // object: the primary key lives on the table.
            is_unique: false,
            is_primary: false,
        });
    }
    Ok(indexes)
}

/// Every column of every relation in one keyspace, in one call.
///
/// The bulk path exists so the tree does not run one statement per table; it is
/// read from the same `system_schema.columns` rows that `table_details` reads.
pub(crate) fn schema_columns(
    connection: &CassandraConnection,
    keyspace: &str,
) -> Result<Vec<SchemaColumnInfo>, DbError> {
    let result = connection.run_catalog_query(
        LIST_SCHEMA_COLUMNS_CQL,
        vec![Value::Text(keyspace.to_string())],
    )?;
    let mut columns = parse_columns(&result.rows)?;
    columns.sort_by(|left, right| {
        left.table
            .cmp(&right.table)
            .then_with(|| column_order(left, right))
    });
    // The bulk read is the keyspace-wide schema load, so it leaves the
    // language service knowing the same keys a per-table read would.
    for (table, keys) in keys_by_table(&columns) {
        connection.remember_key_columns(keyspace, &table, keys);
    }
    // A counter column makes its table read-only. The bulk rows cannot tell a
    // materialized view from a table — a view's columns are here too — so only
    // the counter fact is recorded and the view's own kind comes from the
    // view list or a per-table read.
    for (table, has_counter) in counter_tables(&columns) {
        if has_counter {
            connection.remember_relation(keyspace, &table, RelationKind::CounterTable);
        }
    }
    Ok(columns
        .into_iter()
        .map(|column| SchemaColumnInfo {
            table_name: column.table.clone(),
            column: column_info(&column),
        })
        .collect())
}

/// Columns, key roles and indexes of one table or materialized view.
pub(crate) fn table_details(
    connection: &CassandraConnection,
    keyspace: &str,
    table: &str,
) -> Result<TableInfo, DbError> {
    let binds = vec![
        Value::Text(keyspace.to_string()),
        Value::Text(table.to_string()),
    ];
    let column_rows = connection.run_catalog_query(LIST_COLUMNS_CQL, binds.clone())?;
    let index_result = connection.run_catalog_query(LIST_INDEXES_CQL, binds.clone())?;
    let base_table = base_table_name(connection, keyspace, table)?;

    if column_rows.rows.is_empty() && base_table.is_none() {
        return Err(DbError::ObjectNotFound(
            format!("Cassandra table '{keyspace}.{table}' was not found").into(),
        ));
    }

    let TableLayout {
        columns,
        mut hints,
        keys,
        has_counter,
    } = columns_from_rows(&column_rows.rows)?;
    // The same rows name the key, so the classifier can read a later `DELETE`
    // on this table.
    connection.remember_key_columns(keyspace, table, keys);
    let kind = if base_table.is_some() {
        RelationKind::MaterializedView
    } else if has_counter {
        RelationKind::CounterTable
    } else {
        RelationKind::Table
    };
    connection.remember_relation(keyspace, table, kind);
    if let Some(base_table) = base_table {
        // A materialized view is read-only and its rows are maintained by the
        // base table, so the base table's name is what the user needs next.
        hints.push(TableStorageHint {
            label: "Base table".to_string(),
            columns: Vec::new(),
            detail: Some(base_table),
        });
    }

    Ok(TableInfo {
        name: table.to_string(),
        schema: Some(keyspace.to_string()),
        columns: Some(columns),
        indexes: Some(IndexData::Relational(index_infos(&index_result.rows)?)),
        foreign_keys: None,
        constraints: None,
        sample_fields: None,
        presentation: CollectionPresentation::DataGrid,
        child_items: None,
        storage_hints: Some(hints),
        pseudo_columns: Box::default(),
    })
}

/// A table the tree can list before its columns are read.
fn shallow_table(keyspace: &str, table: &str) -> TableInfo {
    TableInfo {
        name: table.to_string(),
        schema: Some(keyspace.to_string()),
        columns: None,
        indexes: None,
        foreign_keys: None,
        constraints: None,
        sample_fields: None,
        presentation: CollectionPresentation::DataGrid,
        child_items: None,
        storage_hints: None,
        pseudo_columns: Box::default(),
    }
}

pub(crate) fn base_table_name(
    connection: &CassandraConnection,
    keyspace: &str,
    table: &str,
) -> Result<Option<String>, DbError> {
    let result = connection.run_catalog_query(
        VIEW_BASE_TABLE_CQL,
        vec![
            Value::Text(keyspace.to_string()),
            Value::Text(table.to_string()),
        ],
    )?;
    let Some(row) = result.rows.first() else {
        return Ok(None);
    };
    Ok(Some(required_text(row, 0, "base_table_name")?))
}

/// One column as `system_schema.columns` records it.
struct ParsedColumn {
    table: String,
    name: String,
    role: ColumnRole,
    position: i32,
    type_name: String,
    clustering_order: String,
}

impl ParsedColumn {
    /// Whether the column is part of the primary key: a partition-key or a
    /// clustering column.
    fn is_key(&self) -> bool {
        matches!(self.role, ColumnRole::PartitionKey | ColumnRole::Clustering)
    }
}

/// Where a column sits in the CQL key and row layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColumnRole {
    PartitionKey,
    Clustering,
    Static,
    Regular,
}

impl ColumnRole {
    /// The role a `system_schema.columns.kind` cell names.
    ///
    /// A kind this driver does not know is read as a regular column: CQL has
    /// those four today, and marking an unknown column as part of the key would
    /// be a guess, while leaving it listed is always true.
    fn parse(kind: &str) -> Self {
        match kind {
            "partition_key" => Self::PartitionKey,
            "clustering" => Self::Clustering,
            "static" => Self::Static,
            _ => Self::Regular,
        }
    }

    /// The order the tree lists columns in: the key first, as CQL reads it.
    fn rank(self) -> u8 {
        match self {
            Self::PartitionKey => 0,
            Self::Clustering => 1,
            Self::Static => 2,
            Self::Regular => 3,
        }
    }
}

/// What one table's `system_schema.columns` rows say: its columns in tree
/// order, the hints for the key roles and the clustering order, and the primary
/// key itself, partition first and then the clustering columns.
#[derive(Debug)]
pub(crate) struct TableLayout {
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) hints: Vec<TableStorageHint>,
    pub(crate) keys: Vec<String>,
    /// Whether any column of the table is a counter, which makes the whole
    /// table read-only for row edits (a counter changes by increment).
    pub(crate) has_counter: bool,
}

/// Columns, key hints and key of one table from `system_schema.columns` rows,
/// parsed once: the rows are read for the tree and for the classifier alike.
pub(crate) fn columns_from_rows(rows: &[Vec<Value>]) -> Result<TableLayout, DbError> {
    let mut parsed = parse_columns(rows)?;
    parsed.sort_by(column_order);
    let (columns, hints) = column_layout(&parsed);
    let keys = parsed
        .iter()
        .filter(|column| column.is_key())
        .map(|column| column.name.clone())
        .collect();
    let has_counter = parsed.iter().any(|column| column.type_name == COUNTER_TYPE);

    Ok(TableLayout {
        columns,
        hints,
        keys,
        has_counter,
    })
}

/// The key of each table in `sorted`, which must be ordered by table name and
/// then by [`column_order`], so one table's key columns are contiguous and in
/// CQL's key order.
fn keys_by_table(sorted: &[ParsedColumn]) -> Vec<(String, Vec<String>)> {
    let mut layout: Vec<(String, Vec<String>)> = Vec::new();
    for column in sorted.iter().filter(|column| column.is_key()) {
        match layout.last_mut() {
            Some((table, keys)) if table == &column.table => keys.push(column.name.clone()),
            _ => layout.push((column.table.clone(), vec![column.name.clone()])),
        }
    }
    layout
}

/// Whether each table in `sorted` has a counter column, in table order.
fn counter_tables(sorted: &[ParsedColumn]) -> Vec<(String, bool)> {
    let mut facts: Vec<(String, bool)> = Vec::new();
    for column in sorted {
        let is_counter = column.type_name == COUNTER_TYPE;
        match facts.last_mut() {
            Some((table, has_counter)) if table == &column.table => {
                *has_counter = *has_counter || is_counter;
            }
            _ => facts.push((column.table.clone(), is_counter)),
        }
    }
    facts
}

fn parse_columns(rows: &[Vec<Value>]) -> Result<Vec<ParsedColumn>, DbError> {
    rows.iter()
        .map(|row| {
            Ok(ParsedColumn {
                table: required_text(row, 0, "table_name")?,
                name: required_text(row, 1, "column_name")?,
                role: ColumnRole::parse(&required_text(row, 2, "kind")?),
                position: required_position(row, 3)?,
                type_name: required_text(row, 4, "type")?,
                // ScyllaDB answers `DESC` and `ASC` where Cassandra answers
                // `desc` and `asc`, so the order is read in one case and the
                // same table gets the same hint on both servers.
                clustering_order: required_text(row, 5, "clustering_order")?.to_ascii_lowercase(),
            })
        })
        .collect()
}

/// Partition key first by position, then the clustering columns in clustering
/// order, then static and regular columns by name.
fn column_order(left: &ParsedColumn, right: &ParsedColumn) -> std::cmp::Ordering {
    left.role
        .rank()
        .cmp(&right.role.rank())
        .then_with(|| left.position.cmp(&right.position))
        .then_with(|| left.name.cmp(&right.name))
}

fn column_info(column: &ParsedColumn) -> ColumnInfo {
    let is_key = column.is_key();
    ColumnInfo {
        name: column.name.clone(),
        type_name: column.type_name.clone(),
        // CQL has no `NOT NULL`: a column outside the key accepts a null.
        nullable: !is_key,
        is_primary_key: is_key,
        default_value: None,
        enum_values: None,
    }
}

/// The table's columns plus one hint per key role.
///
/// The role is a [`TableStorageHint`] rather than a new core field, so the tree
/// and the details pane show it without any driver-specific UI.
fn column_layout(columns: &[ParsedColumn]) -> (Vec<ColumnInfo>, Vec<TableStorageHint>) {
    let infos = columns.iter().map(column_info).collect();

    let mut hints = Vec::new();
    for (role, label) in [
        (ColumnRole::PartitionKey, "Partition key"),
        (ColumnRole::Clustering, "Clustering key"),
        (ColumnRole::Static, "Static column"),
    ] {
        let names = names_in_role(columns, role);
        if !names.is_empty() {
            hints.push(TableStorageHint {
                label: label.to_string(),
                columns: names,
                detail: None,
            });
        }
    }

    // Only the descending part of the clustering order is worth a row: the rest
    // is the natural ascending order CQL writes.
    let descending: Vec<String> = columns
        .iter()
        .filter(|column| column.role == ColumnRole::Clustering && column.clustering_order == "desc")
        .map(|column| column.name.clone())
        .collect();
    if !descending.is_empty() {
        hints.push(TableStorageHint {
            label: "Clustering order".to_string(),
            columns: descending,
            detail: Some("descending".to_string()),
        });
    }

    (infos, hints)
}

fn names_in_role(columns: &[ParsedColumn], role: ColumnRole) -> Vec<String> {
    columns
        .iter()
        .filter(|column| column.role == role)
        .map(|column| column.name.clone())
        .collect()
}

fn index_infos(rows: &[Vec<Value>]) -> Result<Vec<IndexInfo>, DbError> {
    rows.iter()
        .map(|row| {
            Ok(IndexInfo {
                name: required_text(row, 0, "index_name")?,
                columns: row.get(1).and_then(index_target).into_iter().collect(),
                is_unique: false,
                is_primary: false,
            })
        })
        .collect()
}

/// The column an index targets, read from `system_schema.indexes.options`.
///
/// A secondary index writes the column as `values(col)`, `keys(col)`,
/// `entries(col)` or `full(col)`, and a custom index may hold it alone. An index
/// without a target column (a keyspace-wide custom index) yields none. A custom
/// index whose target is a class name rather than a column is reported as it is
/// written: this driver cannot tell one from a column name without preparing the
/// statement.
fn index_target(options: &Value) -> Option<String> {
    let Value::Document(fields) = options else {
        return None;
    };
    let target = fields.get("target")?;
    target_column_name(&target.as_display_string())
}

pub(crate) fn target_column_name(target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let name = match target.split_once('(') {
        Some((_, inner)) => inner.trim_end_matches(')').trim(),
        None => target,
    };
    let name = name.trim_matches('"');
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// One text column of a catalog answer, every row required.
fn text_column(rows: &[Vec<Value>], index: usize, what: &str) -> Result<Vec<String>, DbError> {
    rows.iter()
        .map(|row| required_text(row, index, what))
        .collect()
}

/// A text cell the browse cannot do without.
///
/// A name or a kind that does not arrive as text means `system_schema` changed
/// under the driver, so the read fails instead of quietly listing fewer objects
/// or forgetting which columns form the key.
fn required_text(row: &[Value], index: usize, what: &str) -> Result<String, DbError> {
    match row.get(index) {
        Some(Value::Text(text)) => Ok(text.clone()),
        other => Err(catalog_shape_error(what, other)),
    }
}

/// The `position` cell: a CQL `int`, negative for a column outside the key.
fn required_position(row: &[Value], index: usize) -> Result<i32, DbError> {
    match row.get(index) {
        // An integer too wide for `i32` is still an integer, so the shape error
        // would name the wrong problem; the value itself is never echoed.
        Some(Value::Int(position)) => i32::try_from(*position).map_err(|_| {
            DbError::query_failed(
                "system_schema returned a position out of range where the driver reads position",
            )
        }),
        other => Err(catalog_shape_error("position", other)),
    }
}

/// Why a catalog cell is not the type this driver reads, without the cell's
/// value: a catalog cell may hold a name from the user's schema.
fn catalog_shape_error(what: &str, cell: Option<&Value>) -> DbError {
    let found = match cell {
        Some(Value::Text(_)) => "text",
        Some(Value::Int(_)) => "an integer",
        Some(Value::Null) | None => "nothing",
        Some(_) => "another type",
    };
    DbError::query_failed(format!(
        "system_schema returned {found} where the driver reads {what}"
    ))
}
