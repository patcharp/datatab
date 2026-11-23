//! CQL text for generated reads and edits.
//!
//! The generator answers the UI's structured reads (MCP's `select_data`, the
//! builder preview) and its mutation text (copy-as-query, the confirmation
//! dialog, the visual mutation run). Everything it emits is CQL: double-quoted
//! identifiers, `?` binds, no `OFFSET`, no `ORDER BY` a browse cannot express,
//! and no joins.
//!
//! The edit text carries the same lightweight-transaction guard the driver's
//! own row edits use, so the statement a user copies is the statement the grid
//! runs.

use datatab_core::{
    AssignmentValue, BoolOp, ColumnAssignment, Comparator, DbError, FilterNode, GeneratedMutation,
    GeneratedQuery, GeneratorError, LiteralValue, MutationCategory, MutationKind, MutationRequest,
    MutationTemplateOperation, MutationTemplateRequest, PlaceholderStyle, PredicateValue,
    Projection, QueryGenError, QueryGenerator, QueryLanguage, ReadTemplateOperation,
    ReadTemplateRequest, SelectQuery, SqlDialect, Value, VisualMutationSpec, VisualQuerySpec,
};

use crate::edit::composite_identity;

/// The one generator this driver hands out.
pub(crate) static GENERATOR: CassandraQueryGenerator = CassandraQueryGenerator;

/// The dialect the connection reports and the generator writes with.
pub(crate) static DIALECT: CassandraDialect = CassandraDialect;

/// CQL: double-quoted identifiers, `?` binds, `0x…` blobs, `[…]` lists.
#[derive(Debug, Default, Clone, Copy)]
pub struct CassandraDialect;

impl SqlDialect for CassandraDialect {
    fn quote_identifier(&self, name: &str) -> String {
        let escaped = name.replace('"', "\"\"");
        format!("\"{escaped}\"")
    }

    fn qualified_table(&self, schema: Option<&str>, table: &str) -> String {
        match schema {
            Some(keyspace) => format!(
                "{}.{}",
                self.quote_identifier(keyspace),
                self.quote_identifier(table)
            ),
            None => self.quote_identifier(table),
        }
    }

    fn value_to_literal(&self, value: &Value) -> String {
        match value {
            Value::Null => "NULL".to_string(),
            Value::Bool(flag) => if *flag { "true" } else { "false" }.to_string(),
            Value::Int(number) => number.to_string(),
            Value::Float(number) => number.to_string(),
            Value::Decimal(decimal) => decimal.clone(),
            Value::Text(text) => format!("'{}'", self.escape_string(text)),
            Value::Bytes(bytes) => {
                let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                format!("0x{hex}")
            }
            Value::Json(json) => format!("'{}'", self.escape_string(json)),
            Value::DateTime(date_time) => {
                format!("'{}'", date_time.format("%Y-%m-%d %H:%M:%S%.f"))
            }
            Value::Date(date) => format!("'{}'", date.format("%Y-%m-%d")),
            Value::Time(time) => format!("'{}'", time.format("%H:%M:%S%.f")),
            Value::Array(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(|item| self.value_to_literal(item))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Document(_) => format!("'{}'", self.escape_string(&value.as_display_string())),
            Value::ObjectId(id) => format!("'{}'", self.escape_string(id)),
            Value::Unsupported(_) => "NULL".to_string(),
        }
    }

    fn escape_string(&self, text: &str) -> String {
        text.replace('\'', "''")
    }

    fn placeholder_style(&self) -> PlaceholderStyle {
        PlaceholderStyle::QuestionMark
    }

    /// CQL has no `OFFSET`.
    ///
    /// `generate_select` and `browse_page_number` refuse a non-zero offset
    /// before any text is built, so a clause asked for one is a caller bug.
    /// This method cannot return an error, so it refuses the only way it can:
    /// it writes the `OFFSET` the caller asked for, which the server rejects,
    /// and logs the bug. Silently returning the first page instead would answer
    /// a different question than the one asked.
    fn select_limit_offset_clause(
        &self,
        limit: Option<u64>,
        offset: u64,
        _has_order_by: bool,
    ) -> Option<String> {
        let limit = limit.filter(|limit| *limit > 0);
        match (limit, offset) {
            (None, 0) => None,
            (Some(limit), 0) => Some(format!("LIMIT {limit}")),
            (limit, offset) => {
                log::error!(
                    "a CQL SELECT was built with OFFSET {offset}, which CQL cannot express"
                );
                let limit_clause = limit.map(|limit| format!("LIMIT {limit}\n"));
                Some(format!(
                    "{}OFFSET {offset}",
                    limit_clause.unwrap_or_default()
                ))
            }
        }
    }
}

/// The CQL generator: reads for MCP and the builder, edit text for the grid.
#[derive(Debug, Default, Clone, Copy)]
pub struct CassandraQueryGenerator;

impl QueryGenerator for CassandraQueryGenerator {
    fn supported_categories(&self) -> &'static [MutationCategory] {
        &[MutationCategory::Sql]
    }

    fn generate_mutation(&self, mutation: &MutationRequest) -> Option<GeneratedQuery> {
        let text = match mutation {
            MutationRequest::SqlInsert(insert) => {
                insert_text(&insert.table, insert.schema.as_deref(), &insert.assignments).ok()?
            }
            MutationRequest::SqlUpdate(patch) => {
                let identity = composite_identity(&patch.identity)?;
                update_text(
                    &patch.table,
                    patch.schema.as_deref(),
                    &patch.changes,
                    &identity,
                )
                .ok()?
            }
            MutationRequest::SqlDelete(delete) => {
                let identity = composite_identity(&delete.identity)?;
                delete_text(&delete.table, delete.schema.as_deref(), &identity).ok()?
            }
            // CQL writes one row per statement: there is no multi-row UPDATE or
            // DELETE, and no upsert-on-conflict.
            _ => return None,
        };
        Some(GeneratedQuery {
            language: QueryLanguage::Cql,
            text,
        })
    }

    fn generate_template(&self, request: &MutationTemplateRequest<'_>) -> Option<GeneratedQuery> {
        let table = DIALECT.qualified_table(request.schema, request.table);
        let key: Vec<&str> = request
            .columns
            .iter()
            .filter(|column| column.is_primary_key)
            .map(|column| column.name.as_str())
            .collect();
        if key.is_empty() {
            return None;
        }
        let key_predicate = key
            .iter()
            .map(|column| format!("{} = ?", DIALECT.quote_identifier(column)))
            .collect::<Vec<_>>()
            .join(" AND ");

        let text = match request.operation {
            MutationTemplateOperation::Insert => {
                let columns: Vec<String> = request
                    .columns
                    .iter()
                    .map(|column| DIALECT.quote_identifier(&column.name))
                    .collect();
                let placeholders = vec!["?"; columns.len()].join(", ");
                format!(
                    "INSERT INTO {table} ({}) VALUES ({placeholders}) IF NOT EXISTS;",
                    columns.join(", ")
                )
            }
            MutationTemplateOperation::Update => {
                let assignments: Vec<String> = request
                    .columns
                    .iter()
                    .filter(|column| !column.is_primary_key)
                    .map(|column| format!("{} = ?", DIALECT.quote_identifier(&column.name)))
                    .collect();
                if assignments.is_empty() {
                    return None;
                }
                format!(
                    "UPDATE {table} SET {} WHERE {key_predicate} IF EXISTS;",
                    assignments.join(", ")
                )
            }
            MutationTemplateOperation::Delete => {
                format!("DELETE FROM {table} WHERE {key_predicate} IF EXISTS;")
            }
        };

        Some(GeneratedQuery {
            language: QueryLanguage::Cql,
            text,
        })
    }

    /// Select by the full key: the template the sidebar writes into the editor.
    fn generate_read_template(&self, request: &ReadTemplateRequest<'_>) -> Option<GeneratedQuery> {
        let table = DIALECT.qualified_table(request.schema, request.table);
        let text = match request.operation {
            ReadTemplateOperation::SelectAll => format!("SELECT * FROM {table};"),
            ReadTemplateOperation::SelectWhere => {
                let key: Vec<&str> = request
                    .columns
                    .iter()
                    .filter(|column| column.is_primary_key)
                    .map(|column| column.name.as_str())
                    .collect();
                let predicate: Vec<String> = key
                    .iter()
                    .map(|column| format!("{} = ?", DIALECT.quote_identifier(column)))
                    .collect();
                if predicate.is_empty() {
                    return None;
                }
                format!("SELECT * FROM {table} WHERE {};", predicate.join(" AND "))
            }
        };
        Some(GeneratedQuery {
            language: QueryLanguage::Cql,
            text,
        })
    }

    /// A structured read as CQL.
    ///
    /// MCP's `select_data` and the builder preview reach this. Joins, grouping,
    /// ordering and an offset are refused with a message that names the CQL
    /// limit rather than dropped from the text.
    fn generate_select(
        &self,
        spec: &VisualQuerySpec,
    ) -> Result<Option<SelectQuery>, QueryGenError> {
        if !spec.joins.is_empty() {
            return Err(QueryGenError::InvalidSpec("CQL has no joins".to_string()));
        }
        if spec.is_grouped() || spec.having.is_some() {
            return Err(QueryGenError::InvalidSpec(
                "CQL has no GROUP BY or HAVING in this driver".to_string(),
            ));
        }
        if !spec.sort.is_empty() {
            return Err(QueryGenError::InvalidSpec(
                "CQL cannot order this view; the grid sorts it in memory".to_string(),
            ));
        }
        if spec.offset != 0 {
            return Err(QueryGenError::InvalidSpec("CQL has no OFFSET".to_string()));
        }

        let projection = match &spec.projection {
            Projection::All => "*".to_string(),
            Projection::Explicit(columns) => {
                if columns.is_empty() {
                    return Err(QueryGenError::InvalidSpec(
                        "a SELECT with no column would read nothing".to_string(),
                    ));
                }
                columns
                    .iter()
                    .map(|column| DIALECT.quote_identifier(&column.column))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };

        let mut params = Vec::new();
        let mut sql = format!(
            "SELECT {projection}\nFROM {}",
            DIALECT.qualified_table(spec.source.schema.as_deref(), &spec.source.table)
        );
        if let Some(filter) = spec.filter.as_ref() {
            sql.push_str("\nWHERE ");
            sql.push_str(&render_filter(filter, &mut params)?);
        }
        if let Some(limit) = spec.limit.filter(|limit| *limit > 0) {
            sql.push_str(&format!("\nLIMIT {limit}"));
        }

        Ok(Some(SelectQuery { sql, params }))
    }

    fn materialize_select_for_editor(&self, query: &SelectQuery) -> String {
        query.materialize_for_editor(&DIALECT)
    }

    /// A visual update as CQL, with the key in its `WHERE` and `IF EXISTS`.
    fn generate_update_from_spec(
        &self,
        spec: &VisualMutationSpec,
    ) -> Result<GeneratedMutation, GeneratorError> {
        let MutationKind::Update { assignments } = &spec.kind else {
            return Err(GeneratorError::Unsupported(
                "DELETE passed to generate_update_from_spec".to_string(),
            ));
        };
        if assignments.is_empty() {
            return Err(GeneratorError::EmptyAssignments);
        }

        let mut params = Vec::new();
        let mut used_raw_expression = false;
        let set: Vec<String> = assignments
            .iter()
            .map(|assignment| {
                let column = DIALECT.quote_identifier(&assignment.column);
                match &assignment.value {
                    AssignmentValue::Literal(literal) => {
                        params.push(scalar_value(literal));
                        format!("{column} = ?")
                    }
                    AssignmentValue::Null => format!("{column} = NULL"),
                    AssignmentValue::Expression(expression) => {
                        used_raw_expression = true;
                        format!("{column} = {expression}")
                    }
                    AssignmentValue::Default => "".to_string(),
                }
            })
            .collect();
        if set.iter().any(String::is_empty) {
            return Err(GeneratorError::Unsupported(
                "CQL has no DEFAULT: name the value to write".to_string(),
            ));
        }

        let table = DIALECT.qualified_table(spec.from.schema.as_deref(), &spec.from.name);
        let where_clause = match spec.filter.as_ref() {
            Some(filter) => render_filter(filter, &mut params)
                .map_err(|error| GeneratorError::Unsupported(error.to_string()))?,
            None => {
                return Err(GeneratorError::Unsupported(
                    "A CQL update needs the row's key in its WHERE clause".to_string(),
                ));
            }
        };

        Ok(GeneratedMutation {
            sql: format!(
                "UPDATE {table}\nSET {}\nWHERE {where_clause} IF EXISTS",
                set.join(", ")
            ),
            params,
            used_raw_expression,
        })
    }

    /// A visual delete as CQL, with `IF EXISTS` and a `WHERE` the server can
    /// address.
    fn generate_delete_from_spec(
        &self,
        spec: &VisualMutationSpec,
    ) -> Result<GeneratedMutation, GeneratorError> {
        let Some(filter) = spec.filter.as_ref() else {
            return Err(GeneratorError::Unsupported(
                "A CQL delete needs a WHERE clause: CQL has no bare DELETE".to_string(),
            ));
        };
        let mut params = Vec::new();
        let where_clause = render_filter(filter, &mut params)
            .map_err(|error| GeneratorError::Unsupported(error.to_string()))?;
        let table = DIALECT.qualified_table(spec.from.schema.as_deref(), &spec.from.name);

        Ok(GeneratedMutation {
            sql: format!("DELETE FROM {table}\nWHERE {where_clause} IF EXISTS"),
            params,
            used_raw_expression: false,
        })
    }
}

fn insert_text(
    table: &str,
    keyspace: Option<&str>,
    assignments: &[ColumnAssignment],
) -> Result<String, DbError> {
    if assignments.is_empty() {
        return Err(DbError::query_failed("An insert needs at least one column"));
    }
    let columns: Vec<String> = assignments
        .iter()
        .map(|assignment| DIALECT.quote_identifier(&assignment.name))
        .collect();
    let values: Vec<String> = assignments
        .iter()
        .map(|assignment| DIALECT.value_to_literal(&assignment.value))
        .collect();
    Ok(format!(
        "INSERT INTO {} ({}) VALUES ({}) IF NOT EXISTS;",
        DIALECT.qualified_table(keyspace, table),
        columns.join(", "),
        values.join(", ")
    ))
}

fn update_text(
    table: &str,
    keyspace: Option<&str>,
    assignments: &[ColumnAssignment],
    identity: &[(String, Value)],
) -> Result<String, DbError> {
    if assignments.is_empty() {
        return Err(DbError::query_failed("An update needs at least one column"));
    }
    let set: Vec<String> = assignments
        .iter()
        .map(|assignment| {
            format!(
                "{} = {}",
                DIALECT.quote_identifier(&assignment.name),
                DIALECT.value_to_literal(&assignment.value)
            )
        })
        .collect();
    Ok(format!(
        "UPDATE {} SET {} WHERE {} IF EXISTS;",
        DIALECT.qualified_table(keyspace, table),
        set.join(", "),
        identity_predicate(identity)?
    ))
}

fn delete_text(
    table: &str,
    keyspace: Option<&str>,
    identity: &[(String, Value)],
) -> Result<String, DbError> {
    Ok(format!(
        "DELETE FROM {} WHERE {} IF EXISTS;",
        DIALECT.qualified_table(keyspace, table),
        identity_predicate(identity)?
    ))
}

fn identity_predicate(identity: &[(String, Value)]) -> Result<String, DbError> {
    if identity.is_empty() {
        return Err(DbError::query_failed(
            "A CQL edit needs the row's key columns and their values",
        ));
    }
    Ok(identity
        .iter()
        .map(|(column, value)| {
            format!(
                "{} = {}",
                DIALECT.quote_identifier(column),
                DIALECT.value_to_literal(value)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND "))
}

/// A filter tree as CQL's `WHERE` terms, binding each value.
///
/// Only what CQL reads is rendered: `=`, `!=`, the range comparisons, `IN`,
/// and `AND`/`OR`. `LIKE`, `IS NULL` and the other shapes are refused with a
/// message instead of being written into a statement the server would reject.
fn render_filter(node: &FilterNode, params: &mut Vec<Value>) -> Result<String, QueryGenError> {
    match node {
        FilterNode::Group { op, children } => {
            if children.is_empty() {
                return Err(QueryGenError::InvalidSpec(
                    "a filter group with no condition".to_string(),
                ));
            }
            let joiner = match op {
                BoolOp::And => " AND ",
                BoolOp::Or => " OR ",
            };
            let parts = children
                .iter()
                .map(|child| render_filter(child, params))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})", parts.join(joiner)))
        }
        FilterNode::Predicate(predicate) => {
            let column = DIALECT.quote_identifier(&predicate.column);
            match (&predicate.comparator, &predicate.value) {
                (Comparator::Eq, PredicateValue::Single(value)) => {
                    params.push(literal_value(value));
                    Ok(format!("{column} = ?"))
                }
                (Comparator::Neq, PredicateValue::Single(value)) => {
                    params.push(literal_value(value));
                    Ok(format!("{column} != ?"))
                }
                (Comparator::Gt, PredicateValue::Single(value)) => {
                    params.push(literal_value(value));
                    Ok(format!("{column} > ?"))
                }
                (Comparator::Gte, PredicateValue::Single(value)) => {
                    params.push(literal_value(value));
                    Ok(format!("{column} >= ?"))
                }
                (Comparator::Lt, PredicateValue::Single(value)) => {
                    params.push(literal_value(value));
                    Ok(format!("{column} < ?"))
                }
                (Comparator::Lte, PredicateValue::Single(value)) => {
                    params.push(literal_value(value));
                    Ok(format!("{column} <= ?"))
                }
                (Comparator::In, PredicateValue::List(values)) => {
                    if values.is_empty() {
                        return Err(QueryGenError::InvalidSpec(
                            "an IN list with no value".to_string(),
                        ));
                    }
                    let slots = vec!["?"; values.len()].join(", ");
                    params.extend(values.iter().map(literal_value));
                    Ok(format!("{column} IN ({slots})"))
                }
                (comparator, _) => Err(QueryGenError::InvalidSpec(format!(
                    "CQL cannot express {comparator:?} in a WHERE clause"
                ))),
            }
        }
    }
}

/// A timestamp the user typed: parsed when it is ISO-8601, otherwise sent as
/// written so the server names the problem.
fn timestamp_value(text: &str) -> Value {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|date_time| Value::DateTime(date_time.with_timezone(&chrono::Utc)))
        .unwrap_or_else(|_| Value::Text(text.to_string()))
}

/// A SET literal as the bound value CQL expects.
fn scalar_value(literal: &datatab_core::ScalarLiteral) -> Value {
    use datatab_core::ScalarLiteral;
    match literal {
        ScalarLiteral::Text(text) => Value::Text(text.clone()),
        ScalarLiteral::Integer(number) => Value::Int(*number),
        ScalarLiteral::Float(number) => Value::Float(*number),
        ScalarLiteral::Bool(flag) => Value::Bool(*flag),
        ScalarLiteral::Timestamp(text) => timestamp_value(text),
        ScalarLiteral::Null => Value::Null,
    }
}

/// A filter literal as the bound value CQL expects.
fn literal_value(literal: &LiteralValue) -> Value {
    match literal {
        LiteralValue::Text(text) => Value::Text(text.clone()),
        LiteralValue::Integer(number) => Value::Int(*number),
        LiteralValue::Float(number) => Value::Float(*number),
        LiteralValue::Bool(flag) => Value::Bool(*flag),
        // CQL takes a timestamp as a bound timestamp, not as text; a value the
        // driver cannot parse goes out as written and the server names the
        // problem.
        LiteralValue::Timestamp(text) => timestamp_value(text),
        LiteralValue::Null => Value::Null,
    }
}
