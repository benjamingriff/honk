# Honk

Honk is a macOS command-line client for read-only analysis of an Athena and
Iceberg lakehouse. It is intended for engineers and coding agents. Honk checks
each SQL statement locally, verifies an explicitly named temporary AWS session,
and keeps result data on stdout while operational details go to stderr.

## Install

Honk requires the Rust toolchain pinned in `rust-toolchain.toml`.

```bash
cargo install --locked --path .
honk --version
```

For development, run the checks used before a release:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo deny check
```

The automated suite does not access AWS.

## Configure a connection

Create `~/.config/honk/config.toml`:

```toml
[connections.dev]
account = "111111111111"
region = "eu-west-1"
role_arn = "arn:aws:iam::111111111111:role/honk-readonly"
workgroup = "analytics-dev"
# Optional defaults. Commands can override these with flags.
default_catalog = "AwsDataCatalog"
default_database = "analytics_dev"
# Optional if the workgroup supplies a result location.
# output_location = "s3://example-athena-results/dev/"
policy = "read_only"
query_timeout = "30m"
```

Replace every example value with the settings for your account. Do not put AWS
credentials in this file. `default_catalog` and `default_database` are optional;
they are conveniences rather than access restrictions. Check the result with:

```bash
honk config check
honk connections
```

## Prepare a session

V1 reads temporary credentials from one explicitly named section of
`~/.aws/credentials`:

```ini
[dev-session]
aws_access_key_id = ...
aws_secret_access_key = ...
aws_session_token = ...
```

Create or refresh that profile with your existing AWS authentication tool. Honk
does not create, refresh, or modify it. It ignores ambient AWS credential
variables and does not follow `source_profile`, `credential_process`, or entries
from `~/.aws/config`.

Check that the profile resolves to the account and role configured for the
connection:

```bash
honk session check --connection dev --session dev-session
```

## Discover data

Discovery uses AWS metadata APIs and does not submit discovery SQL:

```bash
honk catalogs --connection dev --session dev-session
honk databases --connection dev --session dev-session
honk databases --connection dev --session dev-session --catalog AwsDataCatalog
honk tables --connection dev --session dev-session \
  --catalog AwsDataCatalog --database analytics_dev
honk describe --connection dev --session dev-session analytics_dev.orders
```

## Run a query

Honk accepts one statement as an argument, from a file, or from stdin:

```bash
honk query --connection dev --session dev-session \
  "SELECT order_id, status FROM analytics_dev.orders LIMIT 20"

honk query --connection dev --session dev-session \
  --catalog AwsDataCatalog --database analytics_dev \
  "SELECT order_id, status FROM orders LIMIT 20"

honk query --connection dev --session dev-session --file investigation.sql

printf '%s\n' 'SELECT count(*) FROM analytics_dev.orders' | \
  honk query --connection dev --session dev-session --format jsonl
```

The read-only policy allows `SELECT`, `WITH ... SELECT`, `EXPLAIN` without
`ANALYZE`, `SHOW`, and `DESCRIBE`. It rejects writes, DDL, `USE`, procedures,
multiple statements, and anything the policy cannot classify. There is no
override flag.

Namespace flags take precedence over connection defaults. A query may omit both
defaults and flags when its SQL is fully qualified. Discovery commands require
the namespace they need: `databases` needs a catalog; `tables` needs a catalog
and database; and `describe TABLE` needs both. `describe DATABASE.TABLE` supplies
the database itself. Honk reports missing discovery context before contacting
AWS.

## Output

Supported formats are `table`, `csv`, `tsv`, `json`, `jsonl`, and `markdown`.
Direct terminal output defaults to a table. Piped output defaults to JSON Lines.
Agents should request JSON Lines explicitly.

```bash
honk query --connection dev --session dev-session --format jsonl \
  "SELECT * FROM analytics_dev.orders LIMIT 20"

honk query --connection dev --session dev-session --output results.csv \
  "SELECT * FROM analytics_dev.orders LIMIT 1000"
```

For `--output`, Honk infers the format from the extension and writes through a
temporary sibling file. It will not replace an existing file unless you pass
`--force`. `--quiet` suppresses successful operational messages on stderr.

## Failure behavior

Honk uses these exit codes:

| Code | Meaning |
| ---: | --- |
| 0 | Success |
| 2 | Invocation, configuration, or SQL policy error |
| 3 | Authentication or session error |
| 4 | Athena or metadata-provider failure, including timeout |
| 5 | Result formatting or output-file error |
| 130 | Interrupted by the user |

If a running query times out or receives `Ctrl-C`, Honk requests cancellation
and reports the query ID when one exists. Refresh the named profile after an
exit-code 3 error. Honk never prints full SQL, credentials, session tokens, or
panic payloads in its diagnostics.

The complete behavior and design rationale live in [SPEC.md](SPEC.md) and
[IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md).
