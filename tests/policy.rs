use honk::policy::athena::{
    AthenaDialect, MAX_SQL_BYTES, PolicyError, StatementKind, ValidatedQuery,
};
use proptest::prelude::*;
use sqlparser::ast::{SetExpr, Statement as SqlparserStatement};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

struct AllowedCase {
    name: &'static str,
    kind: StatementKind,
    sql: &'static str,
}

const ALLOWED: &[AllowedCase] = &[
    AllowedCase {
        name: "literal select",
        kind: StatementKind::Select,
        sql: "SELECT 1",
    },
    AllowedCase {
        name: "trailing semicolon and comment",
        kind: StatementKind::Select,
        sql: "-- investigation\nSELECT 'DROP TABLE x' AS harmless;",
    },
    AllowedCase {
        name: "quoted cross-catalog table",
        kind: StatementKind::Select,
        sql: r#"SELECT o.order_id FROM "other-catalog"."analytics-db"."orders-table" AS o LIMIT 10"#,
    },
    AllowedCase {
        name: "cte and nested query",
        kind: StatementKind::Select,
        sql: "WITH paid AS (SELECT * FROM orders WHERE status = 'paid') SELECT count(*) FROM paid",
    },
    AllowedCase {
        name: "recursive cte",
        kind: StatementKind::Select,
        sql: "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 10) SELECT * FROM t",
    },
    AllowedCase {
        name: "set operations",
        kind: StatementKind::Select,
        sql: "SELECT id FROM a UNION ALL SELECT id FROM b EXCEPT SELECT id FROM c",
    },
    AllowedCase {
        name: "window query",
        kind: StatementKind::Select,
        sql: "SELECT order_id, row_number() OVER (PARTITION BY customer_id ORDER BY created_at DESC) AS rn FROM orders",
    },
    AllowedCase {
        name: "grouping sets",
        kind: StatementKind::Select,
        sql: "SELECT region, status, count(*) FROM orders GROUP BY GROUPING SETS ((region), (status), ())",
    },
    AllowedCase {
        name: "aggregate filter",
        kind: StatementKind::Select,
        sql: "SELECT count(*) FILTER (WHERE status = 'paid') FROM orders",
    },
    AllowedCase {
        name: "unnest with ordinality",
        kind: StatementKind::Select,
        sql: "SELECT o.id, item, n FROM orders o CROSS JOIN UNNEST(o.items) WITH ORDINALITY AS u(item, n)",
    },
    AllowedCase {
        name: "array lambda",
        kind: StatementKind::Select,
        sql: "SELECT transform(ARRAY[1, 2, 3], x -> x + 1)",
    },
    AllowedCase {
        name: "two argument lambda",
        kind: StatementKind::Select,
        sql: "SELECT reduce(ARRAY[1, 2, 3], 0, (s, x) -> s + x, s -> s)",
    },
    AllowedCase {
        name: "map and row values",
        kind: StatementKind::Select,
        sql: "SELECT MAP(ARRAY['a'], ARRAY[1]), CAST(ROW(1, 'x') AS ROW(id bigint, label varchar))",
    },
    AllowedCase {
        name: "json path string",
        kind: StatementKind::Select,
        sql: "SELECT json_extract_scalar(payload, '$.customer.id') FROM events",
    },
    AllowedCase {
        name: "iceberg metadata table",
        kind: StatementKind::Select,
        sql: r#"SELECT snapshot_id, committed_at FROM analytics."orders$snapshots" ORDER BY committed_at DESC"#,
    },
    AllowedCase {
        name: "hidden path column",
        kind: StatementKind::Select,
        sql: r#"SELECT DISTINCT "$path" FROM analytics.orders"#,
    },
    AllowedCase {
        name: "table sample",
        kind: StatementKind::Select,
        sql: "SELECT * FROM orders TABLESAMPLE SYSTEM (10)",
    },
    AllowedCase {
        name: "safe explain",
        kind: StatementKind::Explain,
        sql: "EXPLAIN SELECT * FROM orders WHERE ds = DATE '2024-01-01'",
    },
    AllowedCase {
        name: "explain options",
        kind: StatementKind::Explain,
        sql: "EXPLAIN (TYPE IO, FORMAT JSON) SELECT * FROM orders",
    },
    AllowedCase {
        name: "show catalogs",
        kind: StatementKind::Show,
        sql: "SHOW CATALOGS",
    },
    AllowedCase {
        name: "show databases",
        kind: StatementKind::Show,
        sql: "SHOW DATABASES",
    },
    AllowedCase {
        name: "show tables",
        kind: StatementKind::Show,
        sql: "SHOW TABLES IN analytics",
    },
    AllowedCase {
        name: "show create table",
        kind: StatementKind::Show,
        sql: "SHOW CREATE TABLE analytics.orders",
    },
    AllowedCase {
        name: "show create view",
        kind: StatementKind::Show,
        sql: "SHOW CREATE VIEW analytics.paid_orders",
    },
    AllowedCase {
        name: "show columns",
        kind: StatementKind::Show,
        sql: "SHOW COLUMNS IN analytics.orders",
    },
    AllowedCase {
        name: "show partitions",
        kind: StatementKind::Show,
        sql: "SHOW PARTITIONS analytics.orders",
    },
    AllowedCase {
        name: "show table properties",
        kind: StatementKind::Show,
        sql: "SHOW TBLPROPERTIES analytics.orders",
    },
    AllowedCase {
        name: "show views",
        kind: StatementKind::Show,
        sql: "SHOW VIEWS IN analytics",
    },
    AllowedCase {
        name: "describe table",
        kind: StatementKind::Describe,
        sql: "DESCRIBE analytics.orders",
    },
    AllowedCase {
        name: "short describe table",
        kind: StatementKind::Describe,
        sql: "DESC analytics.orders",
    },
    AllowedCase {
        name: "formatted describe table",
        kind: StatementKind::Describe,
        sql: "DESCRIBE FORMATTED analytics.orders",
    },
];

const REJECTED: &[(&str, &str)] = &[
    ("blank", "  -- only a comment\n "),
    ("multiple statements", "SELECT 1; SELECT 2"),
    ("insert", "INSERT INTO orders SELECT * FROM staging_orders"),
    ("update", "UPDATE orders SET status = 'paid' WHERE id = 1"),
    ("delete", "DELETE FROM orders WHERE id = 1"),
    (
        "merge",
        "MERGE INTO orders o USING updates u ON o.id = u.id WHEN MATCHED THEN UPDATE SET status = u.status",
    ),
    ("create table", "CREATE TABLE x (id bigint)"),
    ("ctas", "CREATE TABLE x AS SELECT * FROM orders"),
    ("create view", "CREATE VIEW x AS SELECT * FROM orders"),
    ("alter", "ALTER TABLE orders ADD COLUMN note varchar"),
    ("drop", "DROP TABLE orders"),
    ("truncate", "TRUNCATE TABLE orders"),
    ("optimize", "OPTIMIZE orders REWRITE DATA USING BIN_PACK"),
    ("vacuum", "VACUUM orders"),
    ("msck repair", "MSCK REPAIR TABLE orders"),
    (
        "unload",
        "UNLOAD (SELECT * FROM orders) TO 's3://example/results/' WITH (format = 'PARQUET')",
    ),
    (
        "call",
        "CALL system.sync_partition_metadata('analytics', 'orders', 'FULL')",
    ),
    ("prepare", "PREPARE q FROM SELECT * FROM orders"),
    ("execute", "EXECUTE q"),
    ("deallocate prepare", "DEALLOCATE PREPARE q"),
    ("analyze", "ANALYZE orders"),
    ("use", "USE analytics"),
    (
        "set session",
        "SET SESSION join_distribution_type = 'BROADCAST'",
    ),
    ("grant", "GRANT SELECT ON orders TO role analyst"),
    ("start transaction", "START TRANSACTION"),
    ("commit", "COMMIT"),
    ("top-level values", "VALUES (1), (2)"),
    ("top-level table", "TABLE orders"),
    ("select into", "SELECT * INTO copied_orders FROM orders"),
    ("select for update", "SELECT * FROM orders FOR UPDATE"),
    ("explain analyze", "EXPLAIN ANALYZE SELECT * FROM orders"),
    (
        "explain analyze option",
        "EXPLAIN (ANALYZE TRUE) SELECT * FROM orders",
    ),
    (
        "explain insert",
        "EXPLAIN INSERT INTO archive SELECT * FROM orders",
    ),
    (
        "explain ctas",
        "EXPLAIN CREATE TABLE x AS SELECT * FROM orders",
    ),
    (
        "nested second statement",
        "SELECT '; DROP TABLE orders' AS harmless; DROP TABLE orders",
    ),
    (
        "external function",
        "USING EXTERNAL FUNCTION redact(x varchar) RETURNS varchar LAMBDA 'redactor' SELECT redact(email) FROM customers",
    ),
    (
        "create materialized view",
        "CREATE MATERIALIZED VIEW totals AS SELECT count(*) FROM orders",
    ),
    (
        "refresh materialized view",
        "REFRESH MATERIALIZED VIEW totals",
    ),
    (
        "alter view dialect",
        "ALTER VIEW paid_orders DIALECT ATHENA AS SELECT * FROM orders WHERE status = 'paid'",
    ),
];

const KNOWN_PARSER_GAPS: &[(&str, &str)] = &[
    (
        "iceberg timestamp travel",
        "SELECT * FROM iceberg_orders FOR TIMESTAMP AS OF TIMESTAMP '2024-01-01 00:00:00 UTC'",
    ),
    (
        "iceberg expression time travel",
        "SELECT * FROM iceberg_orders FOR TIMESTAMP AS OF (current_timestamp - interval '1' day)",
    ),
    (
        "iceberg version travel",
        "SELECT * FROM iceberg_orders FOR VERSION AS OF 949530903748831860",
    ),
];

fn validate(sql: &str) -> Result<StatementKind, PolicyError> {
    ValidatedQuery::parse(sql.to_owned()).map(|query| query.kind())
}

#[test]
fn corpus_counts_match_the_spike() {
    assert_eq!(ALLOWED.len(), 31);
    assert_eq!(REJECTED.len(), 39);
    assert_eq!(KNOWN_PARSER_GAPS.len(), 3);
}

#[test]
fn representative_athena_reads_are_allowed() {
    let mut failures = Vec::new();
    for case in ALLOWED {
        let result = validate(case.sql);
        if result != Ok(case.kind) {
            failures.push(format!("{}: {result:?}", case.name));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn writes_and_unclassified_statements_are_rejected() {
    for (name, sql) in REJECTED {
        let result = validate(sql);
        assert!(
            result.is_err(),
            "{name} was unexpectedly allowed as {result:?}"
        );
    }
}

#[test]
fn documented_athena_time_travel_has_a_specific_rejection() {
    for (name, sql) in KNOWN_PARSER_GAPS {
        assert_eq!(validate(sql), Err(PolicyError::IcebergTimeTravel), "{name}");
    }
}

#[test]
fn time_travel_words_in_strings_comments_and_identifiers_are_harmless() {
    for sql in [
        "SELECT 'FOR VERSION AS OF'",
        "SELECT 1 -- FOR TIMESTAMP AS OF is text in a comment",
        r#"SELECT "FOR", "VERSION", "AS", "OF" FROM orders"#,
    ] {
        assert_eq!(validate(sql), Ok(StatementKind::Select), "{sql}");
    }
}

#[test]
fn custom_dialect_parses_athena_lambda_as_a_lambda() {
    let sql = "SELECT transform(ARRAY[1, 2, 3], x -> x + 1)";
    let generic = Parser::parse_sql(&GenericDialect, sql).expect("generic parse");
    let athena = Parser::parse_sql(&AthenaDialect::new(), sql).expect("Athena parse");
    assert!(!format!("{generic:#?}").contains("Lambda("));
    assert!(format!("{athena:#?}").contains("Lambda("));
    assert_eq!(validate(sql), Ok(StatementKind::Select));
}

#[test]
fn query_bodied_writes_are_rejected_if_the_parser_accepts_them() {
    for sql in [
        "WITH source AS (SELECT 1 AS id) INSERT INTO target SELECT id FROM source",
        "WITH source AS (SELECT 1 AS id) UPDATE target SET id = source.id FROM source",
        "WITH source AS (SELECT 1 AS id) DELETE FROM target USING source WHERE target.id = source.id",
    ] {
        assert!(
            validate(sql).is_err(),
            "query-bodied write was allowed: {sql}"
        );
    }
}

#[test]
fn with_insert_really_is_a_query_node_in_sqlparser() {
    let sql = "WITH source AS (SELECT 1 AS id) INSERT INTO target SELECT id FROM source";
    let statements = Parser::parse_sql(&AthenaDialect::new(), sql).expect("parse WITH INSERT");
    let SqlparserStatement::Query(query) = &statements[0] else {
        panic!("expected sqlparser to represent WITH INSERT as Query");
    };
    assert!(matches!(query.body.as_ref(), SetExpr::Insert(_)));
    assert!(validate(sql).is_err());
}

#[test]
fn explain_analyze_is_rejected_in_every_supported_spelling() {
    for sql in [
        "EXPLAIN ANALYZE SELECT 1",
        "EXPLAIN (ANALYZE) SELECT 1",
        "EXPLAIN (analyze true, format json) SELECT 1",
        "EXPLAIN (ANALYZE FALSE) SELECT 1",
    ] {
        assert_eq!(validate(sql), Err(PolicyError::ExplainAnalyze), "{sql}");
    }
}

#[test]
fn unknown_explain_options_fail_closed() {
    let result = validate("EXPLAIN (COSTS TRUE) SELECT 1");
    assert_eq!(result, Err(PolicyError::ExplainOptionNotAllowed));
}

#[test]
fn validated_query_retains_the_exact_original_sql() {
    let original = "\n-- keep this comment\nSELECT  1 AS value;\n".to_owned();
    let query = ValidatedQuery::parse(original.clone()).expect("valid query");
    assert_eq!(query.sql(), original);
    assert_eq!(query.kind(), StatementKind::Select);
}

#[test]
fn validated_query_debug_never_contains_sql() {
    let query = ValidatedQuery::parse("SELECT 'DO_NOT_PRINT_ME'".to_owned()).expect("valid query");
    let debug = format!("{query:?}");
    assert!(debug.contains("sql_bytes"));
    assert!(!debug.contains("DO_NOT_PRINT_ME"));
}

#[test]
fn errors_never_echo_sql_tokens() {
    for sql in [
        "SELECT 'DO_NOT_PRINT_ME",
        "CREATE TABLE DO_NOT_PRINT_ME (id bigint)",
        "EXPLAIN (DO_NOT_PRINT_ME TRUE) SELECT 1",
    ] {
        let error = ValidatedQuery::parse(sql.to_owned()).expect_err("rejected SQL");
        assert!(!error.to_string().contains("DO_NOT_PRINT_ME"));
    }
}

#[test]
fn input_larger_than_the_limit_is_rejected_before_parsing() {
    let sql = "x".repeat(MAX_SQL_BYTES + 1);
    assert_eq!(
        ValidatedQuery::parse(sql),
        Err(PolicyError::InputTooLarge {
            limit: MAX_SQL_BYTES
        })
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10_000,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn arbitrary_utf8_never_panics(input in ".{0,512}") {
        let _ = validate(&input);
    }

    #[test]
    fn two_valid_statements_never_become_one(
        left in prop::sample::select(vec!["SELECT 1", "SHOW TABLES", "DESCRIBE orders"]),
        right in prop::sample::select(vec!["SELECT 2", "SHOW CATALOGS", "DESC customers"]),
        whitespace in "[ \\t\\r\\n]{0,8}",
    ) {
        let sql = format!("{left};{whitespace}{right}");
        prop_assert!(validate(&sql).is_err());
    }
}

#[test]
fn excessive_nesting_returns_an_error_instead_of_panicking() {
    let sql = format!("SELECT {}1{}", "(".repeat(1_000), ")".repeat(1_000));
    assert!(validate(&sql).is_err());
}

#[test]
fn parser_is_not_an_athena_semantic_validator() {
    assert_eq!(
        validate("SELECT * FROM 'not-a-table'"),
        Ok(StatementKind::Select)
    );
    assert_eq!(
        validate("DESCRIBE 'not-a-table'"),
        Ok(StatementKind::Describe)
    );
}
