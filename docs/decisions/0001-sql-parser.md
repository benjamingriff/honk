# 0001: use sqlparser-rs for the V1 SQL policy

Status: accepted and implemented

Date: 2026-08-26

Implemented: 2026-09-01

## Decision

Use `sqlparser` 0.62.0 for Honk's V1 SQL policy. Pin the exact version and put a
small Honk-owned `AthenaDialect` in front of it.

The validator will parse exactly one statement and apply a recursive AST
allowlist. It will retain the caller's original SQL for Athena submission. Honk
must never submit SQL regenerated with `Statement::to_string()`.

Do not support Iceberg time travel in the first implementation. Return a clear
local rejection for `FOR TIMESTAMP AS OF` and `FOR VERSION AS OF`. Add support
after `sqlparser` accepts Athena's grammar, preferably through an upstream
change. Do not remove `FOR` from a copy of the query or use token rewriting as a
workaround.

## Why

The spike used Rust 1.98.0 and `sqlparser` 0.62.0. It ran without AWS libraries,
credentials, or live Athena access.

| Experiment | Result |
| --- | --- |
| Representative Athena reads | 31 accepted |
| Documented Iceberg time-travel reads | 3 rejected by the parser |
| Writes, administrative statements, and policy edge cases | 39 rejected |
| Random UTF-8 inputs | 10,000 completed without a panic |
| Generated pairs of valid statements | 10,000 rejected as multiple statements |
| One thousand nested parentheses | Rejected without a panic |
| Formatting and Clippy | Clean with warnings denied |

The accepted reads cover CTEs, recursive CTEs, nested queries, set operations,
windows, grouping sets, aggregate filters, arrays, maps, rows, lambdas,
`UNNEST`, JSON paths, cross-catalog names, Iceberg metadata tables, hidden
columns, table sampling, `EXPLAIN`, `SHOW`, and `DESCRIBE`.

The rejection set covers Athena's DML and DDL families, including CTAS,
`UNLOAD`, `MERGE`, `OPTIMIZE`, `VACUUM`, `MSCK REPAIR`, prepared statements,
external functions, materialized-view operations, transactions, and multiple
statements.

## Findings that affect the implementation

### A query AST is not enough

`sqlparser` represents this input as `Statement::Query`:

```sql
WITH source AS (SELECT 1 AS id)
INSERT INTO target SELECT id FROM source
```

Its query body is `SetExpr::Insert`. The same representation exists for
query-bodied `UPDATE`, `DELETE`, and `MERGE`. A validator that allows every
`Statement::Query` has a write bypass.

Honk must visit every nested `Query` and allow only these set-expression nodes:

- `Select`
- Nested `Query`
- `SetOperation` whose two branches also pass validation

It must reject `Insert`, `Update`, `Delete`, `Merge`, `Values`, and `Table` in
V1. It must also reject `SELECT INTO`, row-locking clauses, query settings,
output `FOR` clauses, format clauses, and pipe operators.

### EXPLAIN needs two checks

The validator must reject the `analyze` field and any utility option named
`ANALYZE`. This catches both forms:

```sql
EXPLAIN ANALYZE SELECT ...
EXPLAIN (ANALYZE TRUE) SELECT ...
```

It must then apply the ordinary query validator to the wrapped statement.
`EXPLAIN INSERT` and `EXPLAIN CREATE TABLE AS SELECT` remain rejected.

V1 allows only Athena's `FORMAT` and `TYPE` options. Unknown options fail
closed.

### Athena needs a small dialect

`GenericDialect` parses a lambda arrow as a different expression node. The
Honk dialect enables lambda functions and the small set of documented query
features exercised by the corpus. It does not claim `GenericDialect` type
identity.

The published `derive_dialect!` macro did not work in an external crate during
the spike. It tried to read `src/dialect/mod.rs` from Honk. Implement the dialect
trait directly until the upstream macro works for consumers.

### Iceberg time travel is the one material gap

Athena requires:

```sql
SELECT * FROM orders FOR TIMESTAMP AS OF TIMESTAMP '2024-01-01 00:00:00 UTC'
SELECT * FROM orders FOR VERSION AS OF 949530903748831860
```

`sqlparser` has table-version AST nodes, but version 0.62.0 accepts only the
Databricks spellings without `FOR`. Its current main branch has the same
behavior and includes a test that rejects `FOR TIMESTAMP AS OF`.

The required parser change is small, but the public dialect trait has no hook
for table-version grammar. Honk would need a fork, a vendored patch, or token
rewriting. None is justified for V1. A clear false rejection is safer.

### Parsing does not prove Athena will accept a query

The parser accepts some syntactically query-shaped input that Athena will
reject, such as a string literal in place of a table name. Honk's policy answers
"could this AST write?" It does not perform catalog-aware or Athena semantic
validation. Athena remains responsible for names, types, functions, and other
execution semantics.

### Dependency upgrades require review

`sqlparser` is pre-1.0 and is adding source spans to its AST. The project warns
that these additions cause breaking pattern-match changes. Pin 0.62.0. For each
upgrade, review the changelog and AST changes, then run the complete policy
corpus before changing the pin.

Keep the wildcard arm in the statement allowlist as a rejection. New statement
variants will then remain blocked even if an upgrade compiles without an
exhaustiveness error.

## Alternative tested

The spike also tested `sqlglot-rust` 0.10.29 because it advertises an Athena
dialect. It is not a good policy parser for Honk V1:

- It splits Athena time travel into a `Select` followed by a raw `FOR` command.
- It parses an Athena lambda arrow as JSON access rather than a lambda.
- It represents both safe `SHOW` and destructive `UNLOAD` as the same raw
  `Command` variant.

Honk would have to inspect command strings and compensate for incorrect AST
nodes. That loses the reason to use an AST policy.

## Production requirements

- Keep parser and policy code independent from the AWS submission code.
- Make the submission API accept a validated-query type, not a raw string.
- Store the original SQL inside that type.
- Require exactly one parsed statement.
- Use explicit arms for every allowed top-level statement.
- Recursively inspect every nested query and set expression.
- Reject unknown nodes and options.
- Sanitize parser errors before printing them. Parser diagnostics can include
  input tokens.
- Keep the corpus in the production test suite and add every real false
  rejection as a regression case.
- Add an input-size limit before parsing so an agent cannot accidentally give
  Honk an enormous file. V1 uses 1,048,576 UTF-8 bytes and bounded reads for
  files and stdin.

## Sources

- [`sqlparser` 0.62.0 documentation](https://docs.rs/sqlparser/0.62.0/sqlparser/)
- [Athena SELECT syntax](https://docs.aws.amazon.com/athena/latest/ug/select.html)
- [Athena DML statement list](https://docs.aws.amazon.com/athena/latest/ug/dml-queries-functions-operators.html)
- [Athena DDL statement list](https://docs.aws.amazon.com/athena/latest/ug/ddl-reference.html)
- [Athena Iceberg time travel](https://docs.aws.amazon.com/athena/latest/ug/querying-iceberg-time-travel-and-version-travel-queries.html)
- [Athena EXPLAIN behavior](https://docs.aws.amazon.com/athena/latest/ug/athena-explain-statement.html)
- [Athena engine version 3 functions](https://docs.aws.amazon.com/athena/latest/ug/functions-env3.html)
