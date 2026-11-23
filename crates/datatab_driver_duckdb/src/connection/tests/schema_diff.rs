use datatab_core::{ColumnInfo, ConstraintInfo, ConstraintKind, TableInfo};

#[test]
fn schema_diff_generators() {
    let connection = super::connect();
    let mut table = table();
    assert_eq!(
        connection
            .generate_code_with_creation_metadata("create_table", &table, None)
            .unwrap(),
        "CREATE TABLE \"Sales\".\"MixedCase\" (\n    \"id\" INTEGER NOT NULL,\n    \"value\" VARCHAR DEFAULT 'x',\n    PRIMARY KEY (\"id\")\n);"
    );
    connection
        .execute(&datatab_core::QueryRequest::new("CREATE SCHEMA \"Sales\""))
        .unwrap();
    connection
        .execute(&datatab_core::QueryRequest::new(
            connection.generate_code("create_table", &table).unwrap(),
        ))
        .unwrap();
    assert_eq!(
        connection.generate_code("drop_table", &table).unwrap(),
        "DROP TABLE \"Sales\".\"MixedCase\";"
    );

    table.constraints = Some(vec![ConstraintInfo {
        name: "unique_value".into(),
        kind: ConstraintKind::Unique,
        columns: vec!["value".into()],
        check_clause: None,
    }]);
    let error = connection
        .generate_code_with_creation_metadata("create_table", &table, None)
        .unwrap_err();
    assert!(error.to_string().contains("unique_value"), "{error}");
}

fn table() -> TableInfo {
    TableInfo {
        name: "MixedCase".into(),
        schema: Some("Sales".into()),
        columns: Some(vec![
            ColumnInfo {
                name: "id".into(),
                type_name: "INTEGER".into(),
                nullable: false,
                is_primary_key: true,
                default_value: None,
                enum_values: None,
            },
            ColumnInfo {
                name: "value".into(),
                type_name: "VARCHAR".into(),
                nullable: true,
                is_primary_key: false,
                default_value: Some("'x'".into()),
                enum_values: None,
            },
        ]),
        indexes: None,
        foreign_keys: None,
        constraints: None,
        sample_fields: None,
        presentation: Default::default(),
        child_items: None,
        storage_hints: None,
        pseudo_columns: Box::default(),
    }
}

#[test]
fn schema_diff_refuses_foreign_key() {
    let connection = super::connect();
    let mut table = table();
    table.foreign_keys = Some(vec![datatab_core::ForeignKeyInfo {
        name: "orders_customer_fk".into(),
        columns: vec!["id".into()],
        referenced_table: "customers".into(),
        referenced_schema: None,
        referenced_columns: vec!["id".into()],
        on_delete: None,
        on_update: None,
    }]);
    let error = connection
        .generate_code("create_table", &table)
        .unwrap_err();
    assert!(error.to_string().contains("orders_customer_fk"), "{error}");
}

#[test]
fn schema_diff_lists_generators() {
    let connection = super::connect();
    let generators = connection.code_generators();
    assert_eq!(
        generators
            .iter()
            .map(|generator| generator.id.as_str())
            .collect::<Vec<_>>(),
        vec!["create_table", "drop_table"]
    );
    assert!(connection.code_gen_capabilities().contains(
        datatab_core::CodeGenCapabilities::CREATE_TABLE
            | datatab_core::CodeGenCapabilities::DROP_TABLE
    ));
}
