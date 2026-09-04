---
name: honk
description: Investigate and export data from an Athena lakehouse through the read-only Honk CLI. Use for lakehouse schema discovery, Athena analysis, and reviewed data exports with Honk. Do not use it for database writes or AWS credential management.
---

# Honk

Use Honk for all lakehouse access in this task. Local tools may process Honk's
results, but do not bypass it with the AWS CLI, an AWS SDK, or another database
client.

## Required task context

The user must supply both a Honk connection name and an AWS session profile.
Pass them on every data command as `--connection` and `--session`. Never infer a
production environment. If either value is missing, ask for it before accessing
AWS. `honk connections` may show configured connection names, but it does not
authorize choosing one.

Catalog and database defaults may come from the connection. Explicit
`--catalog` and `--database` flags override them. Prefer explicit namespace flags
when the user supplies those values or when an investigation crosses namespaces.

## Start safely

- Run `honk config check` when setup is uncertain.
- Run `honk session check --connection CONNECTION --session SESSION` before a
  long or unattended investigation.
- If Honk returns exit code 3 or reports expired credentials, stop and ask the
  user to refresh the named profile. Do not inspect, create, refresh, edit, or
  remove AWS credentials.
- Treat the user's requested environment and data scope as boundaries. Honk's
  read-only SQL policy prevents writes, but it does not grant permission to read
  unrelated data.

## Read the dbt project before querying

Use a dbt project as the first source for model intent, lineage, column meaning,
and likely join keys. Use the path supplied by the user. If no path was supplied,
look for `dbt_project.yml` in the current workspace. Ask for the project path if
you cannot find it. Do not search the user's whole home directory.

This is read-only reference material. Do not edit the project, invoke dbt, or
access AWS through its profiles. Ignore `target/` and `logs/` by default because
they are generated and may be stale.

Start with a narrow search rather than reading the whole project:

```bash
dbt_project=/path/to/dbt-project

rg -n --glob '*.sql' --glob '*.py' --glob '*.yml' --glob '*.yaml' \
  'MODEL_OR_BUSINESS_TERM' "$dbt_project/models"
```

For each relevant model, read only what helps answer the question:

- `dbt_project.yml` for schema, materialization, regional, and environment rules;
- the model's `.sql` or `.py` file for transformations, filters, grain, joins,
  `ref()` calls, and `source()` calls;
- nearby `properties.yml` files for model and column descriptions, tests, and
  constraints;
- nearby `sources.yml` or `sources.yaml` files for source tables and source
  descriptions;
- referenced upstream models when their behavior affects the analysis.

Follow `ref()` and `source()` calls to understand lineage before choosing tables
or join keys. Do not assume a dbt filename maps directly to one Athena database.
Schema names may vary by target, environment, or region.

The dbt checkout describes intended logic. It may be ahead of or behind the
selected environment. Use Honk metadata discovery to confirm the deployed
catalog, database, table, and columns before running SQL. If dbt and Honk differ,
trust Honk for the current physical schema, use dbt for business context, and
mention the mismatch in the final report.

## Investigate

After the dbt review, confirm unfamiliar schemas instead of guessing:

```bash
honk catalogs --connection CONNECTION --session SESSION --format jsonl
honk databases --connection CONNECTION --session SESSION \
  --catalog CATALOG --format jsonl
honk tables --connection CONNECTION --session SESSION \
  --catalog CATALOG --database DATABASE --format jsonl
honk describe --connection CONNECTION --session SESSION \
  --catalog CATALOG DATABASE.TABLE --format jsonl
```

Run one SQL statement per invocation and request JSON Lines explicitly:

```bash
honk query --connection CONNECTION --session SESSION \
  --catalog CATALOG --database DATABASE --format jsonl \
  'SELECT required_columns FROM table_name WHERE partition_key = value LIMIT 100'
```

Start with metadata, aggregates, or small samples. Select only useful columns.
Use partition predicates whenever the schema provides them. Add a practical
`LIMIT` to row-returning investigations, but do not assume that `LIMIT` alone
reduces Athena bytes scanned. Inspect intermediate results before widening the
query.

Stdout is result data. Stderr contains operational information such as the
connection, session, query ID, elapsed time, row count, and bytes scanned. Keep
the streams distinct and do not use `--quiet` when the query ID is needed.

## Export reviewed data

Use an explicit format and Honk's managed file output:

```bash
honk query --connection CONNECTION --session SESSION \
  --catalog CATALOG --database DATABASE --format csv \
  --output /approved/path/result.csv --file /path/to/reviewed.sql
```

Honk writes exports atomically. Do not pass `--force` or replace an existing
file unless the user approved that exact destination. Do not print an export's
rows into the conversation unless the user asked to inspect them.

## Finish the task

Report:

- the connection and session profile used;
- the relevant catalog and database;
- every Athena query ID;
- the findings, caveats, and any output file path;
- any incomplete work caused by session expiry, permissions, timeout, or the
  read-only policy.

Do not include credential values, session tokens, or unnecessary sensitive rows
in the report.
