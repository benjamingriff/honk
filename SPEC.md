# Honk specification

Status: refined working specification

## 1. Purpose

Honk is a private, local command-line tool for authenticated, policy-controlled
access to a company lakehouse built on:

- Amazon Athena for SQL execution
- Apache Iceberg tables queried through Athena
- AWS Glue Data Catalog for metadata
- Separate dev, staging, and production AWS accounts
- MFA-protected role assumption

The installed command is `honk`.

Honk is primarily an interface for coding agents, although it must also work well
for a person at a terminal. It provides a small, deterministic command surface
instead of teaching an agent the company's authentication procedure or giving
it general AWS commands.

The useful comparison is a DuckDB-like CLI for a remote Athena and Iceberg
lakehouse, with explicit AWS session selection, identity verification, and a
mandatory read-only SQL policy built in.

## 2. Primary workflows

### Agent-led investigation

A user asks a coding agent to investigate a question. The agent uses a first-party
Honk skill to discover schemas, run bounded read queries, inspect results, and
report its findings.

```text
User request
    |
    v
Coding agent
    |
    v
Honk commands
    |
    +--> connection resolution
    +--> SQL policy validation
    +--> named AWS session loading
    +--> AWS identity verification
    +--> Athena and Glue
    |
    v
JSON Lines results
    |
    v
Agent analysis
```

Honk never returns an IAM role's temporary credentials to the agent.

### Reviewed data export

A colleague supplies SQL but cannot access the data directly. The user reviews
the statement, runs it through Honk, and returns a CSV file.

```bash
honk query --connection prod --session prod-session \
  --file request.sql --output request.csv
```

Honk validates the statement, verifies the selected AWS session, writes the file
atomically, and reports the Athena query ID and execution statistics on stderr.

## 3. Users and operating environment

V1 has one user and runs on that user's macOS machine.

The design should avoid company-specific assumptions in code so the project can
later support other users, operating systems, and lakehouses. Cross-platform
packaging is not a V1 requirement.

## 4. Threat model

Coding agents are trusted to execute commands. Honk protects against mistakes,
bad generated SQL, accidental environment selection, and accidental credential
exposure. It is not designed to resist an agent or process that is actively
hostile and running as the same macOS user.

A local process running as the same user may be able to read the temporary AWS
credentials profile selected for Honk. Honk must state this limitation clearly.

Security has three layers:

```text
Honk SQL allowlist
        +
AWS IAM permissions
        +
Athena workgroup controls
```

Honk's SQL policy prevents accidental writes and gives useful local errors. IAM
is the final authorization boundary. Athena workgroups enforce query-cost and
result-location controls where configured.

## 5. V1 goals

V1 must:

- Run as a local Rust CLI named `honk`.
- Require a named connection for every data command.
- Require a named, pre-existing AWS session profile for every data command.
- Support dev, staging, and production connections.
- Load temporary credentials only from the explicitly selected session profile.
- Verify the selected session belongs to the configured AWS account and role
  before accessing Athena or Glue.
- Require an explicit `read_only` policy on every connection.
- Parse and validate one SQL statement before submitting it.
- Offer no write policy, override flag, or bypass.
- Submit, poll, time out, and cancel Athena queries.
- Stream paginated Athena results without holding the complete result in memory.
- Write result rows to stdout or an atomically-created file.
- Support table, CSV, TSV, JSON, JSON Lines, and Markdown output.
- Keep result data on stdout and operational information on stderr.
- Discover catalogs, databases, tables, and columns without billed discovery
  queries.
- Report query ID, duration, row count, and bytes scanned.
- Provide deterministic exit codes and errors suitable for agents.
- Ship a first-party coding-agent skill after the command contract is stable.

## 6. V1 non-goals

V1 will not:

- Run write SQL through Honk.
- Provide a human-only policy bypass.
- Export AWS credentials into the caller's environment.
- Reuse credentials already exported into a shell.
- Create, refresh, or delete AWS session profiles.
- Retrieve MFA codes from 1Password.
- Manage dbt models or run dbt.
- Expose an API or MCP server.
- Include an interactive SQL shell.
- Provide a terminal pager.
- Download result objects directly from S3.
- Estimate query scan cost locally.
- Persist a local audit log.
- Run automated tests against a live AWS account.
- Support Windows or Linux packaging.
- Act as a general AWS administration tool.

## 7. Command surface

### Query execution

Exactly one of an inline argument, a file, or stdin supplies SQL.

```bash
honk query --connection dev --session dev-session \
  "SELECT count(*) FROM analytics.orders"
```

```bash
honk query --connection staging --session staging-session \
  --file investigation.sql
```

```bash
cat investigation.sql | honk query --connection prod \
  --session prod-session --format jsonl
```

Useful options:

```text
--connection <name>  required
--session <profile>  required; existing AWS profile with temporary credentials
--file <path>        read SQL from a file
--catalog <name>     override the connection's default catalog
--database <name>    override the connection's default database
--format <format>    table|csv|tsv|json|jsonl|markdown
--output <path>      write result data to a file
--force              replace an existing output file
--quiet              suppress successful operational messages
```

Passing SQL both as an argument and through `--file` or stdin is an invocation
error.

### Metadata discovery

```bash
honk catalogs --connection prod --session prod-session
honk databases --connection prod --session prod-session
honk databases --connection prod --session prod-session --catalog AwsDataCatalog
honk tables --connection prod --session prod-session \
  --catalog AwsDataCatalog --database analytics
honk describe --connection prod --session prod-session analytics.orders
```

Metadata commands accept the same `--format`, `--output`, `--force`, and
`--quiet` behavior as queries.

### Configuration

```bash
honk connections
honk config check
```

`honk connections` lists non-secret connection information. `honk config check`
parses the complete configuration and reports every validation error it can find.
V1 does not edit configuration interactively.

### Session diagnostics

```bash
honk session check --connection prod --session prod-session
```

This command loads the selected profile, calls STS `GetCallerIdentity`, and
checks it against the connection without accessing Athena or Glue. It never
prints credentials or session tokens. V1 has no refresh or logout command
because Honk does not own the profile.

## 8. Configuration

The configuration file is:

```text
~/.config/honk/config.toml
```

V1 has no default connection or session. Every data and session command requires
both `--connection` and `--session`.

An illustrative personal configuration is:

```toml
[connections.dev]
account = "111111111111"
region = "eu-west-1"
role_arn = "arn:aws:iam::111111111111:role/honk-readonly"
workgroup = "analytics-dev"
default_catalog = "AwsDataCatalog"
default_database = "analytics_dev"
# Optional when the workgroup supplies and enforces its own result location.
# output_location = "s3://company-athena-results/dev/"
policy = "read_only"
query_timeout = "30m"

[connections.staging]
account = "222222222222"
region = "eu-west-1"
role_arn = "arn:aws:iam::222222222222:role/honk-readonly"
workgroup = "analytics-staging"
default_catalog = "AwsDataCatalog"
default_database = "analytics_staging"
policy = "read_only"
query_timeout = "30m"

[connections.prod]
account = "333333333333"
region = "eu-west-1"
role_arn = "arn:aws:iam::333333333333:role/honk-readonly"
workgroup = "analytics-prod"
default_catalog = "AwsDataCatalog"
default_database = "analytics"
policy = "read_only"
query_timeout = "15m"
```

The workgroup, namespace defaults, role ARNs, and account-specific values will
be filled in during local setup. The example describes the intended shape, not
shipped defaults. Catalog and database defaults may be omitted.

Configuration rules:

- Unknown fields are errors rather than silently ignored.
- Every connection must set `policy = "read_only"`.
- Missing or unknown policy values are errors.
- Role, region, and workgroup values must be non-empty.
- `default_catalog` and `default_database` are optional, non-empty convenience
  values. The legacy names `catalog` and `database` remain readable for existing
  configs but must not be combined with their replacement names.
- The account in `role_arn` must match the connection's `account` field.
- `output_location` is optional. If it is absent, the workgroup must provide a
  usable result location.
- AWS credentials never belong in this file.

## 9. Authentication and session lifecycle

### Existing AWS session profiles

For V1, the user creates and refreshes a named temporary AWS profile before
running Honk. This may be a profile already prepared for a database client or a
profile created by another local authentication helper. For example:

```text
~/.aws/credentials

[dev-session]
aws_access_key_id = ...
aws_secret_access_key = ...
aws_session_token = ...
```

The `--session <profile>` flag names this AWS profile. A connection and a session
are deliberately separate: the connection selects Athena settings and policy,
while the session selects credentials. Honk does not infer one name from the
other.

Honk constructs the AWS credentials provider for exactly the named section in
`~/.aws/credentials`. V1 does not load `~/.aws/config`, follow source-profile or
role-assumption chains, or run credential processes. `AWS_PROFILE`,
`AWS_CONFIG_FILE`, `AWS_SHARED_CREDENTIALS_FILE`, and exported credential
variables do not redirect this lookup. The selected section must resolve an
access key, secret key, and session token; V1 rejects long-lived static
credentials.

After resolving the profile, Honk retains that exact opaque SDK credential
object for the invocation. A concurrent profile refresh may affect a later
command, but it cannot make one command verify one credential set and use
another.

Before any Athena or Glue call, Honk calls STS `GetCallerIdentity` and verifies:

- The returned account equals the connection's configured `account`.
- The assumed-role name in the returned ARN matches the role name in the
  connection's configured `role_arn`.

The comparison accounts for STS assumed-role ARN syntax, where the role session
name is an additional path component. A missing, incomplete, expired, wrong-
account, or wrong-role profile fails with exit code 3 before lakehouse access.
The error names the profile and tells the user to recreate or refresh it, but
does not print credential material.

### Ownership and lifetime

Honk treats the selected profile as read-only. It does not create, rewrite,
refresh, or remove the profile, and it does not persist separate expiry state.
The user or their existing authentication tooling owns its lifetime.

If the credentials provider exposes an expiry time, Honk rejects a session that
cannot cover the connection's query timeout plus a five-minute safety margin. A
plain shared-credentials profile may not expose that metadata, so
`GetCallerIdentity` can prove only that the session is valid at command start.
V1 cannot guarantee that such a session will remain valid for the whole query.
If it expires mid-command, Honk reports an authentication error and the Athena
query ID; cancellation may also fail because the same credentials have expired.

An agent can work unattended only for as long as the prepared session remains
valid. Session setup is a prerequisite supplied with the task, alongside the
connection and session names. Automatic MFA-backed renewal is the first planned
post-V1 authentication feature.

### Credential handling rules

Honk must never:

- Print credentials or TOTP values.
- Put temporary credentials in result output.
- Export credentials into the parent shell.
- Include secrets in structured errors, debug output, or panic messages.
- Copy credentials into Honk configuration or state files.
- Modify the selected AWS profile.

## 10. SQL policy

### Required policy

V1 supports one policy:

```toml
policy = "read_only"
```

There is no unrestricted policy and no override flag.

### Allowed statements

The allowlist is:

- `SELECT`
- `WITH ... SELECT`
- `EXPLAIN` wrapping an allowed query
- `SHOW`
- `DESCRIBE`
- `DESC`

`EXPLAIN ANALYZE` is not allowed in V1 because it executes the underlying query.

The parser must return exactly one statement. Blank input and multiple statements
are errors.

V1 limits SQL input to 1,048,576 UTF-8 bytes. Honk applies the limit before
parsing and reads at most one byte beyond it from a file or stdin. This catches
accidental oversized input without allocating or parsing the complete file.

### Rejected statements

Everything outside the allowlist is rejected, including:

- `INSERT`
- `UPDATE`
- `DELETE`
- `MERGE`
- `CREATE`
- `CREATE TABLE AS SELECT`
- `ALTER`
- `DROP`
- `TRUNCATE`
- `OPTIMIZE`
- `VACUUM`
- `MSCK REPAIR`
- `UNLOAD`
- `CALL`
- `PREPARE`
- `EXECUTE`
- `DEALLOCATE PREPARE`
- `ANALYZE`
- Top-level `VALUES` and `TABLE`
- Athena external-function declarations

The validator must inspect the complete AST, including statements wrapped by
`EXPLAIN` and query bodies represented by nested AST nodes. It must not rely on
the first keyword or regex matching.

Parse failures and unknown AST variants fail closed. A false rejection of a safe
query is acceptable in the first release. Permitting an unclassified statement
is not.

### Catalog and database access

Honk does not impose a catalog or database allowlist in V1. A read query may use
fully-qualified names and cross-database joins. IAM determines which objects the
selected AWS role can read.

`--catalog` and `--database` select the Athena query execution context and take
precedence over connection defaults. Queries may omit either or both values;
this supports fully-qualified SQL without coupling a connection to one
namespace. Discovery commands fail locally when their required context cannot
be resolved. `databases` requires a catalog, `tables` requires both values, and
an unqualified `describe TABLE` requires both. `describe DATABASE.TABLE`
provides its database and must not be combined with `--database`.

### Parser decision

The completed spike selects `sqlparser` 0.62.0 with an exact version pin and a
small Honk-owned `AthenaDialect`. The dialect enables the documented Athena
features exercised by the corpus, including lambdas, aggregate filters,
grouping sets, and nested comments. Honk implements the dialect trait directly;
the crate's published `derive_dialect!` helper did not work from an external
crate during the spike.

The local experiment accepted 31 representative reads and rejected 39 writes,
administrative statements, and policy edge cases. Twenty thousand generated
cases checked arbitrary UTF-8 input and multiple-statement handling. Excessive
nesting returned an error rather than panicking. No experiment contacted AWS.

The validator cannot treat every `Statement::Query` as safe. `sqlparser`
represents `WITH ... INSERT`, `UPDATE`, `DELETE`, and `MERGE` as query nodes
whose bodies contain write-bearing `SetExpr` variants. Honk must recursively
inspect every query body, CTE, set-operation branch, derived table, and subquery.
It allows only `Select`, nested `Query`, and recursively safe `SetOperation`
nodes. `SELECT INTO`, row-locking clauses, and unclassified query features fail
closed.

Athena Iceberg time travel is the one material parser gap. Version 0.62.0 and
the current upstream branch reject Athena's required `FOR TIMESTAMP AS OF` and
`FOR VERSION AS OF` forms even though the AST has related table-version nodes.
V1 rejects these queries with a specific local error. It will not rewrite tokens
or maintain a parser fork for this feature. Support can follow an upstream parser
change.

Honk retains the original SQL inside the validated-query type and submits that
text to Athena. It never submits SQL regenerated from the AST. Parser upgrades
require an explicit version change and a full corpus run. The detailed evidence
and rejected alternative are in
[`docs/decisions/0001-sql-parser.md`](docs/decisions/0001-sql-parser.md).

## 11. Athena query execution

The execution sequence is:

1. Load and validate configuration.
2. Resolve the named connection.
3. Parse the SQL and apply the read-only policy.
4. Load the explicitly named AWS session profile and verify its identity.
5. Build an Athena client from that session.
6. Call `GetWorkGroup` and verify that the workgroup is enabled and has usable
   result storage.
7. Call `StartQueryExecution` with the workgroup and any resolved catalog or
   database execution context.
8. Poll `GetQueryExecution` until a terminal state.
9. On success, page through `GetQueryResults`.
10. Convert each page into typed rows and write it immediately.
11. Report execution metadata on stderr.

When a workgroup enforces its result configuration, Honk omits the connection's
client-side output location. When the workgroup does not enforce its settings,
the configured connection output location takes precedence. Honk also accepts
workgroups that use Athena-managed query results. It fails before submission if
neither the connection nor workgroup supplies usable result storage.

Honk disables result reuse in each V1 query request. A workgroup may still
override client-side settings where Athena supports that behavior.

Polling should use bounded exponential backoff. The initial implementation may
adopt the AWS JDBC defaults already investigated in Arris: 100 ms minimum, 5
seconds maximum, multiplier 2.

### Timeout and cancellation

Every connection requires a timeout. It covers the complete command, including
submission, polling, result retrieval, and output. The initial personal
configuration uses 30 minutes for dev and staging and 15 minutes for production.

On timeout, `Ctrl-C`, or another local failure after submission, Honk calls
`StopQueryExecution` before exiting if the query is still running. It reports
whether Athena accepted the cancellation request or the request failed. A
cancellation error must not hide the original failure. If expired credentials
prevent cancellation, the error includes the query ID so the user can check it
after refreshing the session.

Honk bounds the cancellation request itself to ten seconds. This prevents a
network failure during `StopQueryExecution` from extending a timed-out command
without limit.

### Result retrieval

V1 uses `GetQueryResults` pagination. Athena's page size is capped at 1,000 rows.
Pagination is invisible to callers and every page is released after formatting.

Direct S3 result download is a later optimization. It adds S3 permissions, CSV
edge cases, and fallback behavior that V1 does not need before real measurements
show an issue.

## 12. Metadata discovery

Discovery must not execute billed `information_schema`, `SHOW`, or `DESCRIBE`
queries when an AWS metadata API can answer the request.

V1 uses:

- Athena `ListDataCatalogs` for catalogs.
- Glue `GetDatabases`, `GetTables`, and `GetTable` for `AwsDataCatalog` metadata.
- Athena `ListDatabases`, `ListTableMetadata`, and `GetTableMetadata` as the
  fallback for non-Glue or federated catalogs.

Metadata includes, where available:

- Catalog and database names
- Tables and views
- Columns and Glue data types
- Partition keys
- S3 locations
- Table parameters
- Iceberg markers

Commands should paginate metadata APIs and stream machine output where practical.
Missing objects and partially unavailable federated catalogs should produce
specific errors without corrupting output from other requests.

## 13. Result formats and values

V1 formats are:

```text
table
csv
tsv
json
jsonl
markdown
```

### Default selection

```text
stdout is a terminal  -> table
stdout is captured    -> jsonl
```

The agent skill always specifies `--format jsonl` rather than depending on TTY
detection.

Table output uses the detected terminal width. Honk renders a horizontal table
when every column can receive a useful minimum width. It truncates long cells
with an ellipsis. If the schema itself is too wide, Honk switches to one
expanded record per row so every column remains visible. An explicitly selected
table written to a non-terminal uses a stable 120-column width.

### JSON shape

JSON and JSON Lines represent each row as an object:

```json
{"order_id":123,"status":"paid","total":"42.00"}
```

Duplicate or blank column labels are made unique deterministically. Duplicate
names gain `_2`, `_3`, and later suffixes. Honk reports renaming on stderr.

JSON is a streamed array. If the process is interrupted, the JSON document may
be incomplete. JSON Lines emits one complete object per line and is the
recommended agent format.

Streaming to stdout can leave partial output after a query, formatter, pipe, or
timeout failure. Callers that require all-or-nothing output should use
`--output`, which writes atomically.

### Type conversion

Athena returns cell contents as text and column types as metadata. Honk converts
using the declared type:

- Boolean values become JSON booleans.
- Integers and floating-point values become JSON numbers when representable.
- Arbitrary-precision decimals remain strings.
- Dates and timestamps remain strings.
- Binary, array, map, and row values remain Athena-formatted strings.
- SQL `NULL` becomes JSON `null`.
- An unparseable typed value falls back to its original string rather than
  failing the complete result.

CSV and TSV retain Athena's textual representation. They include a header row.
CSV uses conventional RFC 4180 quoting. SQL `NULL` is an empty field in V1.

## 14. Stdout and stderr

Result data goes only to stdout unless `--output` is used. Authentication,
progress, warnings, and execution metadata go only to stderr.

Example stderr:

```text
Connection: prod
Session: prod-session
Database: analytics
Query ID: abc123
Status: succeeded
Rows: 42
Elapsed: 1.8s
Data scanned: 23.1 MB
```

`--quiet` suppresses successful operational output. Errors and policy rejections
are never suppressed.

## 15. File output

Shell redirection remains supported:

```bash
honk query --connection prod --session prod-session \
  --format csv "SELECT ..." > results.csv
```

Honk-managed output is safer:

```bash
honk query --connection prod --session prod-session \
  --output results.csv "SELECT ..."
```

Rules:

- A recognized extension selects the format when `--format` is absent.
- A conflicting explicit format and extension is an error.
- Honk writes a temporary file in the destination directory.
- It renames the temporary file only after the query and formatter finish.
- An existing destination is an error unless `--force` is supplied.
- A failure removes the temporary file where possible.
- Honk reports the final output path on stderr.

## 16. Cost controls and execution metadata

V1 relies on Athena workgroups for bytes-scanned limits and other enforced cost
controls. Honk does not attempt to predict scan cost from SQL.

Honk reports actual bytes scanned after execution. Query timeout remains a
client control, while scan limits remain a workgroup control. These concepts are
separate from the read-only SQL policy.

## 17. Audit behavior

V1 writes no persistent local audit log. Athena query history is the execution
record. Honk prints the Athena query ID so activity can be located in AWS.

Honk never logs complete SQL locally by default. Athena itself receives and may
retain the submitted SQL according to AWS-side settings.

## 18. Exit codes

V1 uses stable exit categories:

```text
0    success
2    invocation, configuration, or policy error
3    authentication or session error
4    Athena query failure or timeout
5    result formatting or output-file error
130  interrupted by the user
```

Errors name the connection and operation where useful, but never include secret
values.

## 19. Implementation choices

Honk is implemented in Rust as one binary.

Rust was chosen for:

- Memory safety around credentials and streamed data.
- Strong types for configuration, policy states, query states, and formats.
- Exhaustive handling of AWS terminal states and SQL AST variants.
- A standalone installation with no language runtime.
- A good future open-source distribution path.

Expected building blocks include:

- `clap` for the command interface
- `tokio` for asynchronous AWS calls, polling, signals, and streaming
- `aws-config` and the official STS, Athena, and Glue SDK crates
- `sqlparser = "=0.62.0"` with Honk's `AthenaDialect` and AST visitor
- `serde` and `toml` for strict configuration
- `serde_json` plus CSV and table formatters
- An atomic-write implementation for result files

Honk may reuse design findings and test cases from the Arris Athena work. It will
not depend on Arris or copy AGPL source into a differently licensed project
without resolving licensing first.

## 20. Testing strategy

Automated tests do not call live AWS services.

The suite includes:

- Configuration parsing and validation tests.
- A large SQL policy corpus containing allowed and rejected statements.
- Explicit profile-selection tests that prove ambient AWS environment variables
  cannot redirect credential loading.
- Session-validation tests for missing, incomplete, static, expired,
  wrong-account, and wrong-role profiles.
- Mocked Athena submission, polling, failure, timeout, and cancellation tests.
- Mocked result pagination, header handling, NULL handling, and type conversion.
- Formatter snapshot tests for every format and awkward column names.
- Atomic file output and overwrite-protection tests.
- Mocked Athena and Glue metadata pagination tests.
- Secret-redaction tests for errors and debug output.
- CLI process tests covering stdout, stderr, and exit codes.

Before V1 is declared usable, a manual verification checklist runs against the
real dev environment, followed by limited staging and production checks. These
checks are never part of the default automated test command.

## 21. Agent skill

The repository ships a first-party Honk skill once the command interface is
stable. It tells coding agents to:

- Always name a connection.
- Always name the session profile supplied by the user.
- Always request JSON Lines for analysis.
- Read a relevant dbt project's source files for model intent, lineage, and
  descriptions before querying unfamiliar data.
- Use metadata commands before guessing schemas.
- Keep investigative queries bounded with predicates, aggregation, or `LIMIT`
  where practical.
- Use `--output` and an explicit format for requested exports.
- Treat stderr as operational context and stdout as data.
- Never create, refresh, inspect, or remove AWS credentials itself.
- Ask the user for a refreshed session profile if Honk reports that it has
  expired.
- Report the connection, session profile, and Athena query IDs used in its final
  analysis.

The live command sequence and context-free agent acceptance prompts live in
[`docs/manual-release-checklist.md`](docs/manual-release-checklist.md). Automated
tests never run this checklist because it requires prepared AWS sessions and
real environment knowledge.

## 22. Deferred work

After V1, evidence may justify:

- Honk-managed, automatically refreshed MFA sessions. This is the first planned
  fast follow and begins with a 1Password spike covering service-account
  availability, dedicated-vault scoping, unattended TOTP retrieval, token
  storage, restart and locked-app behavior, and secret-safe failure handling.
- `auth status`, `auth refresh`, and `auth logout` commands for profiles owned by
  Honk, if the spike supports the design.
- Direct S3 result fetching for large exports.
- A default connection.
- A terminal pager.
- Row display limits or fetch limits.
- An interactive shell.
- More metadata and DDL reconstruction.
- Additional authentication providers, including IAM Identity Center.
- Linux and Windows packaging.
- Homebrew or release-binary distribution.
- Local audit logging with query hashes.
- More policy types, only if a real use case requires them.

There is no planned unrestricted mode.

## 23. Release gates

V1 is ready only when:

- The production validator preserves the spike's accepted-read and rejected-
  statement corpus, nested-query checks, and generated-input checks.
- Athena Iceberg time travel produces the documented unsupported-syntax error
  until parser support is added.
- Every connection rejects missing or non-read-only policy configuration.
- Every data command requires both an explicit connection and session profile.
- Session validation rejects missing, static, expired, wrong-account, and
  wrong-role credentials without accessing Athena or Glue.
- Unit and CLI tests pass without live AWS access.
- Manual dev verification covers session validation, discovery, query execution,
  pagination, timeout, cancellation, and every output format.
- Manual staging and production checks confirm the configured roles, workgroups,
  accounts, and read-only IAM behavior.
- No command prints or exports credentials.
- The agent skill completes a real investigation using only Honk.

## 24. Product principle

Honk gives an agent a narrow, useful interface to the lakehouse. Session
selection, identity verification, environment selection, SQL restrictions, and
AWS details remain inside the tool. The agent gets query results, not general
AWS access.
