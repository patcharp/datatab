use super::super::{bulk_column_rows, bulk_index_rows, index_key_part_label};
use datatab_core::ColumnInfo;
use mysql::{Column, Value, consts::ColumnType, mysql_common::row::new_row};
use std::sync::Arc;

fn index_row(
    name: &str,
    column: Option<&str>,
    expression: Option<&str>,
    non_unique: u64,
) -> mysql::Row {
    let text = |value: Option<&str>| {
        value.map_or(Value::NULL, |value| Value::Bytes(value.as_bytes().to_vec()))
    };
    let mut names = vec!["TABLE_NAME", "INDEX_NAME", "COLUMN_NAME", "NON_UNIQUE"];
    let mut values = vec![
        text(Some("people")),
        text(Some(name)),
        text(column),
        Value::UInt(non_unique),
    ];
    if expression.is_some() {
        names.push("EXPRESSION");
        values.push(text(expression));
    }
    let columns: Arc<[Column]> = names
        .into_iter()
        .map(|name| Column::new(ColumnType::MYSQL_TYPE_VAR_STRING).with_name(name.as_bytes()))
        .collect();
    new_row(values, columns)
}

#[test]
fn schema_bulk_columns_decode_catalog_rows() {
    let rows = vec![
        (
            "people".into(),
            "id".into(),
            "int".into(),
            "NO".into(),
            None,
            Some("PRI".into()),
        ),
        (
            "teams".into(),
            "role".into(),
            "enum('a','b')".into(),
            "YES".into(),
            Some("a".into()),
            None,
        ),
    ];
    let expected = ColumnInfo {
        name: "id".into(),
        type_name: "int".into(),
        nullable: false,
        default_value: None,
        is_primary_key: true,
        enum_values: None,
    };
    let result = bulk_column_rows(rows);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].table_name, "people");
    assert_eq!(result[0].column.name, "id");
    assert_eq!(result[0].column.type_name, "int");
    assert!(!result[0].column.nullable);
    assert!(result[0].column.is_primary_key);
    assert_eq!(format!("{:?}", result[0].column), format!("{expected:?}"));
    assert_eq!(result[1].table_name, "teams");
    assert_eq!(
        result[1].column.enum_values.as_deref(),
        Some(["a".to_string(), "b".to_string()].as_slice())
    );
    assert_eq!(result[1].column.default_value.as_deref(), Some("a"));
    let indexes = bulk_index_rows(vec![index_row("ordinary", Some("name"), None, 0)]);
    assert_eq!(indexes.len(), 1);
    assert_eq!(
        indexes[0].columns,
        vec![index_key_part_label(Some("name".into()), None).unwrap()]
    );
    assert!(indexes[0].is_unique);
    assert!(!indexes[0].is_primary);
    assert_eq!(indexes[0].table_name, "people");
}

#[test]
fn schema_bulk_indexes_keep_expression_parts() {
    let rows = vec![index_row("functional", None, Some("lower(`name`)"), 1)];
    let indexes = bulk_index_rows(rows);
    assert_eq!(indexes.len(), 1);
    let expression = indexes
        .iter()
        .find(|index| index.name == "functional")
        .expect("expression index");
    assert_eq!(
        expression.columns,
        vec![index_key_part_label(None, Some("lower(`name`)".into())).unwrap()]
    );
    assert!(!expression.is_unique);
    assert!(!expression.is_primary);
}
