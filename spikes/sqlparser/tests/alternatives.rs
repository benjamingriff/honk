use sqlglot_rust::parser::parse_statements;
use sqlglot_rust::{Dialect, Statement};

#[test]
fn sqlglot_rust_splits_athena_time_travel_into_two_statements() {
    let statements = parse_statements(
        "SELECT * FROM orders FOR VERSION AS OF 949530903748831860",
        Dialect::Athena,
    )
    .unwrap();

    assert_eq!(statements.len(), 2);
    assert!(matches!(statements[0], Statement::Select(_)));
    assert!(matches!(statements[1], Statement::Command(_)));
}

#[test]
fn sqlglot_rust_misclassifies_athena_lambda_arrow_as_json_access() {
    let statements = parse_statements(
        "SELECT transform(ARRAY[1, 2, 3], x -> x + 1)",
        Dialect::Athena,
    )
    .unwrap();
    let debug = format!("{statements:#?}");

    assert!(debug.contains("JsonAccess"));
    assert!(!debug.contains("Lambda"));
}

#[test]
fn sqlglot_rust_uses_the_same_raw_command_node_for_show_and_unload() {
    for sql in [
        "SHOW TABLES IN analytics",
        "UNLOAD (SELECT * FROM orders) TO 's3://example/results/' WITH (format = 'PARQUET')",
    ] {
        let statements = parse_statements(sql, Dialect::Athena).unwrap();
        assert_eq!(statements.len(), 1);
        assert!(matches!(statements[0], Statement::Command(_)), "{sql}");
    }
}

#[test]
fn sqlglot_rust_does_detect_multiple_statements_and_cte_insert() {
    let multiple = parse_statements("SELECT 1; DROP TABLE orders", Dialect::Athena).unwrap();
    assert_eq!(multiple.len(), 2);

    let insert = parse_statements(
        "WITH source AS (SELECT 1 AS id) INSERT INTO target SELECT id FROM source",
        Dialect::Athena,
    )
    .unwrap();
    assert_eq!(insert.len(), 1);
    assert!(matches!(insert[0], Statement::Insert(_)));
}
