use super::*;

#[test]
fn catalog_foreign_key_actions_match_in_both_reads() {
    let actions = [
        ("NO_ACTION", None),
        ("CASCADE", Some("CASCADE")),
        ("SET_NULL", Some("SET NULL")),
        ("SET_DEFAULT", Some("SET DEFAULT")),
    ];
    for (delete, expected_delete) in actions {
        for (update, expected_update) in actions {
            let row: Vec<Value> = ["fk", "child_id", "dbo", "parent", "id", delete, update]
                .into_iter()
                .map(|value| Value::Text(value.into()))
                .collect();
            let table = decode_table_foreign_keys(vec![row.clone()]);
            let mut schema_row = vec![Value::Text("child".into())];
            schema_row.extend(row);
            let schema = decode_schema_foreign_keys(vec![schema_row]);
            assert_eq!(table.len(), 1);
            assert_eq!(schema.len(), 1);
            assert_eq!(
                table[0].on_delete.as_deref(),
                expected_delete,
                "delete={delete}, update={update}"
            );
            assert_eq!(
                table[0].on_update.as_deref(),
                expected_update,
                "delete={delete}, update={update}"
            );
            assert_eq!(table[0].on_delete, schema[0].on_delete);
            assert_eq!(table[0].on_update, schema[0].on_update);
        }
    }
}

#[test]
fn bulk_column_rows_match_per_table_columns() {
    let rows = vec![
        vec![
            Value::Text("id".into()),
            Value::Text("int".into()),
            Value::Bool(false),
            Value::Null,
            Value::Int(1),
            Value::Int(4),
            Value::Int(10),
            Value::Int(0),
        ],
        vec![
            Value::Text("parent_id".into()),
            Value::Text("int".into()),
            Value::Bool(true),
            Value::Null,
            Value::Int(0),
            Value::Int(4),
            Value::Int(10),
            Value::Int(0),
        ],
    ];
    let expected = decode_table_columns(rows.clone());
    let bulk_rows = rows
        .into_iter()
        .map(|row| {
            let mut bulk = vec![Value::Text("child".into())];
            bulk.extend(row);
            bulk
        })
        .collect();
    let actual = decode_schema_columns(bulk_rows);
    assert_eq!(actual.len(), expected.len());
    for (bulk, table) in actual.iter().zip(expected) {
        assert_eq!(bulk.table_name, "child");
        assert_eq!(bulk.column.name, table.name);
        assert_eq!(bulk.column.type_name, table.type_name);
        assert_eq!(bulk.column.nullable, table.nullable);
        assert_eq!(bulk.column.is_primary_key, table.is_primary_key);
        assert_eq!(bulk.column.default_value, table.default_value);
        assert_eq!(bulk.column.enum_values, table.enum_values);
    }
}
