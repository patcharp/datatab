//! The `Connection` implementation for Cassandra and ScyllaDB over CQL.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use datatab_core::{
    Connection, CrudResult, CustomTypeInfo, DatabaseInfo, DbError, DbKind, DbSchemaInfo,
    DefaultSqlDialect, DriverMetadata, LanguageService, QueryHandle, QueryRequest, QueryResult,
    RelationalSchema, RowDelete, RowInsert, RowPatch, SchemaColumnInfo, SchemaFeatures,
    SchemaForeignKeyInfo, SchemaIndexInfo, SchemaLoadingStrategy, SchemaSnapshot,
    SchemaSnapshotAuthority, SqlDialect, TableBrowseRequest, TableCountRequest, TableInfo, Value,
};
use scylla::client::session::Session;
use scylla::frame::types::SerialConsistency;
use scylla::response::PagingState;
use scylla::statement::Statement;
use scylla::value::Row as CqlRow;

use crate::driver::METADATA;
use crate::edit::{self, EditStatement, RelationKind, composite_identity};
use crate::error_formatter::CassandraErrorFormatter;
use crate::language_service::{ActiveKeyspace, CqlLanguageService};
use crate::paging::{BrowseKey, PagingCache, read_grid_page};
use crate::schema;
use crate::session::Worker;
use crate::types::{column_meta_from_spec, value_from_cql, values_to_cql};

/// Rows fetched per page while collecting a result.
///
/// Paging keeps a large result off the wire in one response: a caller's row
/// limit stops the loop at its budget instead of pulling everything first.
const PAGE_SIZE: i32 = 500;

/// The page size for the next request: never more than the rows the budget
/// still allows, and at least one row, because a page that is asked for at the
/// budget's end decides whether the result was longer than the caller wanted.
pub(crate) fn next_page_size(rows_collected: usize, budget: Option<usize>) -> i32 {
    let Some(budget) = budget else {
        return PAGE_SIZE;
    };
    let remaining = budget.saturating_sub(rows_collected);
    let wanted = i32::try_from(remaining).unwrap_or(PAGE_SIZE);
    wanted.clamp(1, PAGE_SIZE)
}

pub struct CassandraConnection {
    /// Declared before `worker` on purpose: fields drop in declaration order,
    /// and the session's connections must be gone before the runtime that
    /// drives them.
    session: Session,
    worker: Arc<Worker>,
    /// The keyspace the session runs in, shared with the language service so a
    /// bare table name in a statement is judged where the statement will run.
    active_keyspace: ActiveKeyspace,
    /// Read by the CQL classifier and written by every schema read that parses
    /// `system_schema.columns`: a `DELETE` is only as safe as the key it names.
    language_service: CqlLanguageService,
    /// Where each page of a browse starts, per relation, filter and page size.
    /// CQL has no `OFFSET`, so this is what makes page 2 and a return to page 1
    /// read the rows they should.
    paging: RwLock<PagingCache>,
    /// What each relation this session has read is, for the edit path: a table
    /// accepts row edits, a materialized view and a counter table do not.
    relations: RwLock<HashMap<(String, String), RelationKind>>,
}

impl CassandraConnection {
    pub(crate) fn new(worker: Arc<Worker>, session: Session) -> Self {
        let active_keyspace: ActiveKeyspace = Arc::new(RwLock::new(
            session.get_keyspace().map(|name| name.to_string()),
        ));
        Self {
            session,
            worker,
            language_service: CqlLanguageService::new(Arc::clone(&active_keyspace)),
            active_keyspace,
            paging: RwLock::new(PagingCache::default()),
            relations: RwLock::default(),
        }
    }

    /// Tells the language service which columns make up one table's primary
    /// key, so a later `DELETE` on that table is read as keyed or partial.
    ///
    /// Every schema read that parses `system_schema.columns` calls this with
    /// the rows it already has; no read of the catalog runs for it.
    pub(crate) fn remember_key_columns(&self, keyspace: &str, table: &str, columns: Vec<String>) {
        self.language_service
            .remember_key_columns(keyspace, table, columns);
    }

    /// Records what a relation is, so a later edit is refused before it runs
    /// when the relation cannot take one.
    pub(crate) fn remember_relation(&self, keyspace: &str, table: &str, kind: RelationKind) {
        let mut relations = self
            .relations
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        relations.insert((keyspace.to_string(), table.to_string()), kind);
    }

    /// What this session knows a relation to be, or `None` when it has not read
    /// it yet.
    fn relation_kind(&self, keyspace: &str, table: &str) -> Option<RelationKind> {
        self.relations
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(keyspace.to_string(), table.to_string()))
            .copied()
    }

    /// What a relation is and which columns form its key, reading the catalog
    /// when this session has not seen it.
    ///
    /// The normal path costs nothing: opening a table reads its details, which
    /// record both. The catalog read is for the first edit of a table this
    /// session reached another way, and it is one per table per session.
    fn relation_for(
        &self,
        keyspace: &str,
        table: &str,
    ) -> Result<(RelationKind, Vec<String>), DbError> {
        let keys = self.language_service.key_columns(keyspace, table);
        if let (Some(kind), Some(keys)) = (self.relation_kind(keyspace, table), keys) {
            return Ok((kind, keys));
        }

        let binds = vec![
            Value::Text(keyspace.to_string()),
            Value::Text(table.to_string()),
        ];
        let column_rows = self.run_catalog_query(schema::LIST_COLUMNS_CQL, binds)?;
        let base_table = schema::base_table_name(self, keyspace, table)?;
        if column_rows.rows.is_empty() && base_table.is_none() {
            return Err(DbError::ObjectNotFound(
                format!("Cassandra table '{keyspace}.{table}' was not found").into(),
            ));
        }
        let layout = schema::columns_from_rows(&column_rows.rows)?;
        let kind = if base_table.is_some() {
            RelationKind::MaterializedView
        } else if layout.has_counter {
            RelationKind::CounterTable
        } else {
            RelationKind::Table
        };
        self.remember_key_columns(keyspace, table, layout.keys.clone());
        self.remember_relation(keyspace, table, kind);
        Ok((kind, layout.keys))
    }

    /// The keyspace an edit runs in: the table's own, or the session's.
    fn edit_keyspace(&self, schema: Option<&str>) -> Result<String, DbError> {
        schema
            .map(str::to_string)
            .or_else(|| self.active_database())
            .ok_or_else(|| {
                DbError::NotSupported(
                    "A Cassandra edit needs the keyspace the table lives in".to_string(),
                )
            })
    }

    /// Runs one prepared lightweight transaction and turns a refused one into
    /// an error.
    ///
    /// The statement is prepared so the server knows every bound type, which a
    /// text statement cannot carry, and its serial consistency is
    /// `LOCAL_SERIAL`: a Paxos round then stays inside the local datacenter
    /// instead of crossing to a remote one for every row edit.
    ///
    /// `what` names the edit the user asked for, never the row: a message about
    /// a failed edit must not carry row values.
    fn run_edit(&self, statement: &EditStatement, what: &str) -> Result<(), DbError> {
        let started = Instant::now();
        let values = values_to_cql(&statement.values)?;
        let mut prepared = self
            .worker
            .block_on(self.session.prepare(statement.cql.clone()))
            .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
        prepared.set_serial_consistency(Some(SerialConsistency::LocalSerial));
        let page = self
            .worker
            .block_on(self.session.execute_unpaged(&prepared, values))
            .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
        let result = lightweight_result(page, started)?;
        match edit::not_applied(&result, what) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Runs one statement, page by page, and materializes it.
    ///
    /// A row limit ends the loop as soon as the budget is reached; the result
    /// is then marked truncated so the grid can say rows were dropped.
    fn run(&self, request: &QueryRequest) -> Result<QueryResult, DbError> {
        // A statement runs in the session's keyspace. `QueryRequest::database`
        // is honoured through `set_active_database`, which is how the console
        // switches keyspace before running; a document that owns a keyspace per
        // tab is a later change, not part of this driver.
        let started = Instant::now();
        let values = values_to_cql(&request.params)?;
        let budget = request.limit.map(|limit| limit as usize);
        let mut statement = Statement::new(request.sql.clone());

        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut truncated = false;
        let mut paging_state = PagingState::start();

        loop {
            statement.set_page_size(next_page_size(rows.len(), budget));
            let (page, paging_response) = self
                .worker
                .block_on(self.session.query_single_page(
                    statement.clone(),
                    values.clone(),
                    paging_state,
                ))
                .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;

            if !page.is_rows() {
                // INSERT, UPDATE, DELETE and DDL return no rows and no count:
                // CQL answers with a void result.
                let mut result = QueryResult::empty();
                result.execution_time = started.elapsed();
                return Ok(result);
            }

            let rows_result = page
                .into_rows_result()
                .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
            if columns.is_empty() {
                columns = rows_result
                    .column_specs()
                    .iter()
                    .map(column_meta_from_spec)
                    .collect();
            }

            let page_rows = rows_result
                .rows::<CqlRow>()
                .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
            let control_flow = paging_response.into_paging_control_flow();

            for row in page_rows {
                let row = row.map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
                // A row exists that the budget cannot hold: the result is
                // longer than what the caller asked for.
                if budget.is_some_and(|budget| rows.len() >= budget) {
                    truncated = true;
                    break;
                }
                rows.push(
                    row.columns
                        .iter()
                        .map(|column| column.as_ref().map(value_from_cql).unwrap_or(Value::Null))
                        .collect(),
                );
            }

            if truncated {
                break;
            }
            match control_flow {
                ControlFlow::Continue(next) => paging_state = next,
                ControlFlow::Break(()) => break,
            }
        }

        let mut result = QueryResult::table(columns, rows, None, started.elapsed());
        result.set_rows_truncated(truncated);
        Ok(result)
    }

    /// Runs one catalog statement to completion, with binds.
    ///
    /// Catalog reads are small and fully qualified, so they need neither a row
    /// budget nor a page walk. The rows are mapped like any other result, which
    /// keeps one value path for the grid and the tree.
    pub(crate) fn run_catalog_query(
        &self,
        cql: &str,
        params: Vec<Value>,
    ) -> Result<QueryResult, DbError> {
        let mut request = QueryRequest::new(cql);
        request.params = params;
        self.run(&request)
    }

    /// The page of a table the request asks for, read through the server's
    /// paging state.
    ///
    /// `Pagination` counts `OFFSET`, which CQL does not have, so the offset is
    /// read as a page number and answered from the state each page starts at.
    /// The statement carries no `LIMIT`: a CQL `LIMIT` bounds the whole query,
    /// so a `LIMIT 100` would leave page 2 empty.
    fn browse_page(&self, request: &TableBrowseRequest) -> Result<QueryResult, DbError> {
        let (target_page, limit) =
            browse_page_number(request.pagination.limit(), request.pagination.offset())?;

        let started = Instant::now();
        let query = browse_cql(request)?;
        let cql = query.cql.clone();
        let key = self.browse_key(request, query.filter);

        // A poisoned lock means another thread panicked holding it: the map
        // itself is still consistent, so the state is recovered rather than
        // thrown away and the page read from the start again.
        let (start_page, start_state) = self
            .paging
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .start_for(&key, target_page)
            .unwrap_or((0, PagingState::start()));

        let mut columns = Vec::new();
        let mut fetch = |page_size: i32, state: PagingState| {
            let mut statement = Statement::new(cql.clone());
            statement.set_page_size(page_size);
            let (page, paging) = self
                .worker
                .block_on(self.session.query_single_page(
                    statement,
                    Vec::<Option<scylla::value::CqlValue>>::new(),
                    state,
                ))
                .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
            let control_flow = paging.into_paging_control_flow();
            let rows_result = page
                .into_rows_result()
                .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
            if columns.is_empty() {
                columns = rows_result
                    .column_specs()
                    .iter()
                    .map(column_meta_from_spec)
                    .collect();
            }
            let rows = rows_result
                .rows::<CqlRow>()
                .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?
                .map(|row| {
                    row.map(|row| {
                        row.columns
                            .iter()
                            .map(|column| {
                                column.as_ref().map(value_from_cql).unwrap_or(Value::Null)
                            })
                            .collect()
                    })
                    .map_err(|error| CassandraErrorFormatter::into_query_error(&error))
                })
                .collect::<Result<Vec<Vec<Value>>, DbError>>()?;
            match control_flow {
                ControlFlow::Continue(next) => Ok((rows, Some(next))),
                ControlFlow::Break(()) => Ok((rows, None)),
            }
        };

        let mut discovered = Vec::new();
        let read = read_grid_page(
            start_page,
            start_state,
            target_page,
            limit,
            &mut fetch,
            &mut |page, state| discovered.push((page, state)),
        )?;

        let mut cache = self
            .paging
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (page, state) in discovered {
            cache.insert(&key, page, state);
        }
        if let Some(next) = &read.next {
            cache.insert(&key, target_page + 1, next.clone());
        }

        Ok(QueryResult::table(
            columns,
            read.rows,
            None,
            started.elapsed(),
        ))
    }

    /// What the paging cache tells pages apart by: the relation, the filter the
    /// statement actually carries, the projection and the page size.
    fn browse_key(&self, request: &TableBrowseRequest, filter: Option<String>) -> BrowseKey {
        BrowseKey::new(
            request
                .table
                .schema
                .clone()
                .or_else(|| self.active_database()),
            request.table.name.clone(),
            filter,
            request.columns.clone(),
            request.pagination.limit(),
        )
    }
}

/// A lightweight transaction's answer as this driver's result: the one row
/// carrying the `[applied]` flag, or an empty result when the server sent none.
fn lightweight_result(
    page: scylla::response::query_result::QueryResult,
    started: Instant,
) -> Result<QueryResult, DbError> {
    if !page.is_rows() {
        return Ok(QueryResult::empty());
    }
    let rows_result = page
        .into_rows_result()
        .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?;
    let columns = rows_result
        .column_specs()
        .iter()
        .map(column_meta_from_spec)
        .collect();
    let rows = rows_result
        .rows::<CqlRow>()
        .map_err(|error| CassandraErrorFormatter::into_query_error(&error))?
        .map(|row| {
            row.map(|row| {
                row.columns
                    .iter()
                    .map(|column| column.as_ref().map(value_from_cql).unwrap_or(Value::Null))
                    .collect()
            })
            .map_err(|error| CassandraErrorFormatter::into_query_error(&error))
        })
        .collect::<Result<Vec<Vec<Value>>, DbError>>()?;
    Ok(QueryResult::table(columns, rows, None, started.elapsed()))
}

/// The page number and row size a browse asks for.
/// CQL has no `OFFSET`, so the offset is read as a page number. An offset that
/// is not a whole number of pages has no paging state to start from, so it is
/// refused instead of answered with the wrong rows.
pub(crate) fn browse_page_number(limit: u32, offset: u64) -> Result<(u64, usize), DbError> {
    let Some(size) = usize::try_from(limit).ok().filter(|size| *size > 0) else {
        return Err(DbError::NotSupported(
            "A Cassandra browse needs a page size above zero".to_string(),
        ));
    };
    if !offset.is_multiple_of(u64::from(limit)) {
        return Err(DbError::NotSupported(
            "Cassandra pages by whole pages; the row offset must be a multiple of the page size"
                .to_string(),
        ));
    }
    Ok((offset / u64::from(limit), size))
}

/// Why a row count is refused: a CQL count is a full scan, which is not a row
/// count but a read of the whole table.
pub(crate) fn row_count_refusal() -> DbError {
    DbError::NotSupported(
        "Cassandra cannot count the rows of a table without scanning it".to_string(),
    )
}

/// A table browse's statement and the `WHERE` it carries.
pub(crate) struct BrowseQuery {
    pub(crate) cql: String,
    /// The filter as the statement writes it. A paging state belongs to the
    /// statement that produced it, so this is what the cache keys on: two
    /// filters that render alike are one statement.
    pub(crate) filter: Option<String>,
}

/// The statement a table browse runs.
///
/// No `LIMIT` and no `OFFSET`: the paging state carries where a page starts, and
/// a CQL `LIMIT` would bound the whole query instead of the page. No `ORDER BY`
/// either: CQL orders a partition by its clustering columns, and ordering by
/// anything else needs a filter that pins the partition first. The grid offers
/// no header sort for a driver without `SORTING`, which this one is.
pub(crate) fn browse_cql(request: &TableBrowseRequest) -> Result<BrowseQuery, DbError> {
    let dialect = DefaultSqlDialect;
    let mut cql = format!(
        "SELECT {} FROM {}",
        request.select_list_with(&dialect),
        request.table.quoted_with(&dialect)
    );

    let filter = if let Some(filter) = request.semantic_filter.as_ref() {
        Some(datatab_core::render_semantic_filter_sql(filter, &dialect)?)
    } else {
        request
            .filter
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };

    if let Some(filter) = &filter {
        cql.push_str(" WHERE ");
        cql.push_str(filter);
    }

    Ok(BrowseQuery { cql, filter })
}

impl Connection for CassandraConnection {
    fn metadata(&self) -> &DriverMetadata {
        &METADATA
    }

    fn ping(&self) -> Result<(), DbError> {
        // `system.local` is served by the coordinator itself, so this answers
        // without touching user data.
        self.worker
            .block_on(
                self.session
                    .query_unpaged("SELECT release_version FROM system.local", &[]),
            )
            .map(|_| ())
            .map_err(|error| CassandraErrorFormatter::into_connection_error(&error))
    }

    fn close(&mut self) -> Result<(), DbError> {
        // Dropping the session closes its pools; there is no protocol-level
        // goodbye in CQL.
        Ok(())
    }

    fn execute(&self, request: &QueryRequest) -> Result<QueryResult, DbError> {
        // A CQL connection has no read-only mode (it has no transactions and no
        // session-level read-only flag), so a request that needs one is refused
        // before anything runs.
        request.refuse_read_only_enforcement("Cassandra")?;
        if request.statement_timeout.is_some() {
            return Err(DbError::NotSupported(
                "Cassandra cannot enforce a per-statement timeout yet".to_string(),
            ));
        }
        let result = self.run(request);
        // A lightweight transaction answers with its `[applied]` row instead of
        // failing, and `false` means the guard refused the edit. That is an
        // error the user must see, never a success: the statement ran, so the
        // classifier still reads it, and nothing was written.
        let refusal = match &result {
            Ok(page) => edit::not_applied(page, "The statement"),
            Err(_) => None,
        };
        // A statement that ran may have switched the session's keyspace or
        // rewritten a table's key, and the classifier must not keep either
        // answer. A statement that failed may still have been applied — a DDL
        // that timed out, a write that reached its replicas after the
        // coordinator gave up — so it clears the keys too, and moves no
        // keyspace. Only the run path invalidates: the editor classifies text
        // as it is typed.
        match &result {
            Ok(_) => self.language_service.statement_ran(&request.sql),
            Err(_) => self.language_service.statement_failed(&request.sql),
        }
        match refusal {
            Some(error) => Err(error),
            None => result,
        }
    }

    /// CQL has no request id the driver can send a cancel for, and a statement
    /// already handed to the session runs to completion or to its timeout, so
    /// there is no client-side cancellation to offer either.
    fn cancel(&self, _handle: &QueryHandle) -> Result<(), DbError> {
        Err(DbError::NotSupported(
            "Cassandra cannot cancel a statement that is already running".to_string(),
        ))
    }

    /// The snapshot lists the keyspaces only; their objects load through
    /// `schema_for_database`. Declaring it authoritative would make the sidebar
    /// show the profile's own keyspace as empty.
    fn schema_snapshot_authority(&self) -> SchemaSnapshotAuthority {
        SchemaSnapshotAuthority::EnumerationOnly
    }

    fn schema(&self) -> Result<SchemaSnapshot, DbError> {
        // The keyspaces are the whole snapshot at this level: objects load per
        // keyspace, as `SchemaLoadingStrategy::LazyPerDatabase` says.
        Ok(SchemaSnapshot::relational(RelationalSchema {
            databases: schema::list_keyspaces(self)?,
            current_database: self.active_database(),
            schemas: Vec::new(),
            tables: Vec::new(),
            views: Vec::new(),
        }))
    }

    fn list_databases(&self) -> Result<Vec<DatabaseInfo>, DbError> {
        schema::list_keyspaces(self)
    }

    fn schema_for_database(&self, database: &str) -> Result<DbSchemaInfo, DbError> {
        schema::schema_for_database(self, database)
    }

    fn table_details(
        &self,
        database: &str,
        _schema: Option<&str>,
        table: &str,
    ) -> Result<TableInfo, DbError> {
        // A keyspace has no inner schema layer, so `database` is the keyspace
        // and `schema` is unused.
        schema::table_details(self, database, table)
    }

    fn schema_types(
        &self,
        database: &str,
        _schema: Option<&str>,
    ) -> Result<Vec<CustomTypeInfo>, DbError> {
        schema::user_types(self, database)
    }

    fn schema_indexes(
        &self,
        database: &str,
        _schema: Option<&str>,
    ) -> Result<Vec<SchemaIndexInfo>, DbError> {
        schema::schema_indexes(self, database)
    }

    fn schema_columns(
        &self,
        database: &str,
        _schema: Option<&str>,
    ) -> Result<Vec<SchemaColumnInfo>, DbError> {
        schema::schema_columns(self, database)
    }

    fn schema_foreign_keys(
        &self,
        _database: &str,
        _schema: Option<&str>,
    ) -> Result<Vec<SchemaForeignKeyInfo>, DbError> {
        // CQL has no foreign key, so the empty answer is the answer. Overriding
        // the default keeps the bulk `schema_columns` contract explicit.
        Ok(Vec::new())
    }

    fn schema_features(&self) -> SchemaFeatures {
        // User-defined types are this driver's custom-type kind. CQL has no
        // foreign key, check constraint, unique constraint, trigger, sequence or
        // stored routine, so nothing else is claimed.
        SchemaFeatures::CUSTOM_TYPES
    }

    /// Pages a table through the server's paging state. The default browse
    /// builds `LIMIT .. OFFSET ..`, and CQL has no `OFFSET`.
    fn browse_table(&self, request: &TableBrowseRequest) -> Result<QueryResult, DbError> {
        self.browse_page(request)
    }

    /// A CQL row count is a full scan of the table (Cassandra has no
    /// `COUNT(*)` aggregate outside a partition), so the driver refuses it
    /// rather than read every row. The grid pages by row count when the count
    /// is refused (`navigation.rs`, `can_go_next`).
    fn count_table(&self, _request: &TableCountRequest) -> Result<u64, DbError> {
        Err(row_count_refusal())
    }
    fn kind(&self) -> DbKind {
        DbKind::Cassandra
    }

    /// Updates one row through a lightweight transaction.
    ///
    /// `UPDATE … IF EXISTS` fails instead of upserting, so an edit of a row that
    /// vanished cannot create it again. The row is addressed by its whole key:
    /// a `WHERE` that stops at the partition key would write every row of the
    /// partition.
    fn update_row(&self, patch: &RowPatch) -> Result<CrudResult, DbError> {
        let keyspace = self.edit_keyspace(patch.schema.as_deref())?;
        let (kind, key_columns) = self.relation_for(&keyspace, &patch.table)?;
        if let Some(refusal) = kind.edit_refusal() {
            return Err(refusal);
        }
        let identity = composite_identity(&patch.identity).ok_or_else(edit::identity_refusal)?;
        let statement = edit::update_statement(
            &keyspace,
            &patch.table,
            &key_columns,
            &identity,
            &patch.changes,
        )?;
        self.run_edit(&statement, "The row update")?;
        Ok(CrudResult::new(1, None))
    }

    /// Inserts one row through a lightweight transaction.
    ///
    /// `INSERT … IF NOT EXISTS` fails instead of overwriting, so an insert of a
    /// key that exists cannot replace its row.
    fn insert_row(&self, insert: &RowInsert) -> Result<CrudResult, DbError> {
        let keyspace = self.edit_keyspace(insert.schema.as_deref())?;
        let (kind, key_columns) = self.relation_for(&keyspace, &insert.table)?;
        if let Some(refusal) = kind.edit_refusal() {
            return Err(refusal);
        }
        let statement =
            edit::insert_statement(&keyspace, &insert.table, &key_columns, &insert.assignments)?;
        self.run_edit(&statement, "The row insert")?;
        Ok(CrudResult::new(1, None))
    }

    /// Deletes one row through a lightweight transaction.
    fn delete_row(&self, delete: &RowDelete) -> Result<CrudResult, DbError> {
        let keyspace = self.edit_keyspace(delete.schema.as_deref())?;
        let (kind, key_columns) = self.relation_for(&keyspace, &delete.table)?;
        if let Some(refusal) = kind.edit_refusal() {
            return Err(refusal);
        }
        let identity = composite_identity(&delete.identity).ok_or_else(edit::identity_refusal)?;
        let statement = edit::delete_statement(&keyspace, &delete.table, &key_columns, &identity)?;
        self.run_edit(&statement, "The row delete")?;
        Ok(CrudResult::new(1, None))
    }

    /// CQL is classified by the driver's own service: the shared heuristics do
    /// not read CQL, so without it a plain `SELECT` dead-ends at the
    /// conservative `Write` default. The service belongs to the connection
    /// because it holds the primary keys the schema reads have returned.
    fn language_service(&self) -> &dyn LanguageService {
        &self.language_service
    }

    /// CQL quotes identifiers with double quotes, marks binds with `?` and has
    /// no `OFFSET`.
    fn dialect(&self) -> &dyn SqlDialect {
        &crate::generator::DIALECT
    }

    /// The CQL generator: structured reads for MCP and the builder, and the
    /// edit text the grid copies and runs.
    fn query_generator(&self) -> Option<&dyn datatab_core::QueryGenerator> {
        Some(&crate::generator::GENERATOR)
    }

    fn schema_loading_strategy(&self) -> SchemaLoadingStrategy {
        // A keyspace is the database layer, so the sidebar loads one on click.
        SchemaLoadingStrategy::LazyPerDatabase
    }

    fn set_active_database(&self, database: Option<&str>) -> Result<(), DbError> {
        let Some(keyspace) = database.map(str::trim).filter(|name| !name.is_empty()) else {
            return Ok(());
        };
        // Keyspace names come from the catalog and from `system_schema`, so they
        // are used as written: `case_sensitive` quotes the name instead of
        // folding it to lower case.
        self.worker
            .block_on(self.session.use_keyspace(keyspace.to_string(), true))
            .map_err(|error| CassandraErrorFormatter::into_connection_error(&error))?;
        match self.active_keyspace.write() {
            Ok(mut active) => *active = Some(keyspace.to_string()),
            Err(error) => {
                return Err(DbError::connection_failed(format!(
                    "The active keyspace could not be recorded: {error}"
                )));
            }
        }
        Ok(())
    }

    fn active_database(&self) -> Option<String> {
        self.active_keyspace
            .read()
            .ok()
            .and_then(|active| active.clone())
    }
}
