use datatab_core::{ColumnInfo, Connection, ConstraintInfo, ConstraintKind, TableInfo};

#[test]
fn schema_diff_generators() {
    let connection = Box::new(super::local_connection("http://127.0.0.1:8123"));
    let mut table = table();
    assert_eq!(
        connection
            .generate_code_with_creation_metadata("create_table", &table, None)
            .unwrap(),
        "CREATE TABLE `Main`.`MixedCase` (\n    `id` UInt64,\n    `value` Nullable(String) DEFAULT 'x'\n) ENGINE = MergeTree ORDER BY (`id`);"
    );
    assert_eq!(
        connection.generate_code("drop_table", &table).unwrap(),
        "DROP TABLE IF EXISTS `Main`.`MixedCase`;"
    );

    table.columns.as_mut().unwrap()[0].is_primary_key = false;
    assert!(
        connection
            .generate_code("create_table", &table)
            .unwrap()
            .ends_with("ORDER BY tuple();")
    );

    table.columns.as_mut().unwrap()[1].type_name = "Nullable(String)".into();
    assert!(
        connection
            .generate_code("create_table", &table)
            .unwrap()
            .contains("`value` Nullable(String)")
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
        schema: Some("Main".into()),
        columns: Some(vec![
            ColumnInfo {
                name: "id".into(),
                type_name: "UInt64".into(),
                nullable: false,
                is_primary_key: true,
                default_value: None,
                enum_values: None,
            },
            ColumnInfo {
                name: "value".into(),
                type_name: "String".into(),
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
    let connection = Box::new(super::local_connection("http://127.0.0.1:8123"));
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
    let connection = Box::new(super::local_connection("http://127.0.0.1:8123"));
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

#[test]
fn schema_diff_preserves_sorting_key() {
    let connection = Box::new(super::local_connection("http://127.0.0.1:8123"));
    let mut table = table();
    table.columns.as_mut().unwrap()[0].is_primary_key = false;
    table.storage_hints = Some(vec![datatab_core::TableStorageHint {
        label: "Sorting Key".into(),
        columns: vec![],
        detail: Some("id, value".into()),
    }]);
    table
        .storage_hints
        .as_mut()
        .unwrap()
        .push(datatab_core::TableStorageHint {
            label: "Engine".into(),
            columns: vec![],
            detail: Some("MergeTree".into()),
        });
    let sql = connection.generate_code("create_table", &table).unwrap();
    assert!(sql.ends_with("ORDER BY (id, value);"), "{sql}");
    table.storage_hints.as_mut().unwrap()[0].detail = Some("(id, value)".into());
    let sql = connection.generate_code("create_table", &table).unwrap();
    assert!(sql.ends_with("ORDER BY (id, value);"), "{sql}");
    table.columns.as_mut().unwrap()[0].is_primary_key = true;
    let sql = connection.generate_code("create_table", &table).unwrap();
    assert!(sql.ends_with("ORDER BY (id, value);"), "{sql}");
}

#[test]
fn schema_diff_refuses_other_engine() {
    let connection = Box::new(super::local_connection("http://127.0.0.1:8123"));
    let mut table = table();
    table.storage_hints = Some(vec![datatab_core::TableStorageHint {
        label: "Engine".into(),
        columns: vec![],
        detail: Some("ReplacingMergeTree".into()),
    }]);
    let error = connection
        .generate_code("create_table", &table)
        .unwrap_err();
    assert!(error.to_string().contains("ReplacingMergeTree"), "{error}");
}

#[test]
fn schema_diff_cloud_engines() {
    let connection = Box::new(super::local_connection("http://127.0.0.1:8123"));
    let mut table = table();
    table.storage_hints = Some(vec![
        datatab_core::TableStorageHint {
            label: "Engine".into(),
            columns: vec![],
            detail: None,
        },
        datatab_core::TableStorageHint {
            label: "Sorting Key".into(),
            columns: vec![],
            detail: Some("id, value".into()),
        },
    ]);
    for engine in [
        "MergeTree",
        "ReplacingMergeTree",
        "SummingMergeTree",
        "AggregatingMergeTree",
        "CollapsingMergeTree",
        "VersionedCollapsingMergeTree",
        "GraphiteMergeTree",
        "ReplicatedMergeTree",
        "ReplicatedReplacingMergeTree",
        "ReplicatedSummingMergeTree",
        "ReplicatedAggregatingMergeTree",
        "ReplicatedCollapsingMergeTree",
        "ReplicatedVersionedCollapsingMergeTree",
        "ReplicatedGraphiteMergeTree",
    ] {
        table.storage_hints.as_mut().unwrap()[0].detail = Some(format!("Shared{engine}"));
        let sql = connection.generate_code("create_table", &table).unwrap();
        assert!(
            sql.ends_with(&format!("ENGINE = {engine} ORDER BY (id, value);")),
            "{sql}"
        );
    }
    table.storage_hints.as_mut().unwrap()[0].detail = Some("SharedUnknownMergeTree".into());
    let error = connection
        .generate_code("create_table", &table)
        .unwrap_err();
    assert!(
        error.to_string().contains("SharedUnknownMergeTree"),
        "{error}"
    );
}
