# Honk V1 manual release checklist

Run this checklist from a clean build after the automated suite passes. These
commands contact AWS. Start in dev and use known low-cost tables and partition
predicates. Record the date, Honk commit, Athena engine version, connection, and
session profile with the results.

## Setup

Install the current checkout and define the values used below:

```bash
cargo install --locked --path . --force

export HONK_CONNECTION=dev
export HONK_SESSION=dev-session
export HONK_CATALOG=AwsDataCatalog
export HONK_DATABASE=YOUR_DATABASE
export HONK_TABLE=YOUR_TABLE
export HONK_PARTITION_PREDICATE='YOUR_PARTITION_COLUMN = YOUR_SAFE_VALUE'

honk --version
honk config check
honk connections
honk session check \
  --connection "$HONK_CONNECTION" \
  --session "$HONK_SESSION"
```

Record a checksum so the final check can prove Honk did not modify the shared
credentials file:

```bash
credentials_checksum_before="$(shasum -a 256 "$HOME/.aws/credentials")"
```

## Session behavior

- [ ] The prepared dev session passes `honk session check`.
- [ ] A nonexistent profile fails with exit code 3.
- [ ] A known static profile fails with exit code 3 before Athena access.
- [ ] Available expired, wrong-account, and wrong-role test profiles each fail
      with exit code 3. Do not create or weaken a real role merely for this test.

Test the missing profile without changing credential files:

```bash
honk session check \
  --connection "$HONK_CONNECTION" \
  --session honk-profile-that-does-not-exist
test "$?" -eq 3
```

## Discovery

```bash
honk catalogs \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --format jsonl

honk databases \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --format jsonl

honk tables \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --database "$HONK_DATABASE" --format jsonl

honk describe \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --format jsonl \
  "$HONK_DATABASE.$HONK_TABLE"
```

- [ ] Catalog, database, table, and column discovery return expected metadata.
- [ ] No discovery command creates an Athena query execution.
- [ ] Iceberg tables and partition keys have the expected markers.

## Query shapes

The first commands exercise syntax without scanning a lake table:

```bash
honk query \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --database "$HONK_DATABASE" --format jsonl \
  'SELECT 1 AS test_value'

honk query \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --database "$HONK_DATABASE" --format jsonl \
  'WITH left_side AS (SELECT 1 AS id), right_side AS (SELECT 1 AS id) SELECT left_side.id, row_number() OVER (ORDER BY left_side.id) AS row_number FROM left_side JOIN right_side ON left_side.id = right_side.id'
```

Use a real Iceberg table for the remaining checks:

```bash
honk query \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --database "$HONK_DATABASE" --format jsonl \
  "SELECT count(*) AS row_count FROM \"$HONK_TABLE\" WHERE $HONK_PARTITION_PREDICATE"

honk query \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --database "$HONK_DATABASE" --format jsonl \
  "SELECT * FROM \"$HONK_TABLE\" WHERE $HONK_PARTITION_PREDICATE LIMIT 1200" \
  | wc -l
```

- [ ] Simple, CTE, join, aggregate, window, and Iceberg reads succeed.
- [ ] A partition containing at least 1,200 rows makes the pagination check print
      `1200`.
- [ ] Successful operations report query IDs and bytes scanned on stderr.

## Output and atomic files

Use cheap constant queries to check every formatter:

```bash
release_directory="$(mktemp -d)"

for format in table csv tsv json jsonl markdown; do
  honk query \
    --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
    --catalog "$HONK_CATALOG" --database "$HONK_DATABASE" \
    --format "$format" \
    --output "$release_directory/result.$format" \
    'SELECT 1 AS test_value, NULL AS empty_value'
done

ls -lh "$release_directory"
```

Check overwrite protection and a successful CSV export:

```bash
printf '%s\n' sentinel > "$release_directory/existing.csv"

honk query \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --format csv --output "$release_directory/existing.csv" \
  'SELECT 1 AS test_value'
test "$?" -eq 2
test "$(cat "$release_directory/existing.csv")" = sentinel

honk query \
  --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
  --catalog "$HONK_CATALOG" --database "$HONK_DATABASE" \
  --format csv --output "$release_directory/export.csv" \
  "SELECT * FROM \"$HONK_TABLE\" WHERE $HONK_PARTITION_PREDICATE LIMIT 100"
```

- [ ] Every output file parses as its named format.
- [ ] An existing destination stays unchanged without `--force`.
- [ ] A completed CSV appears at its final path with no sibling `.honk-*.tmp`
      file left behind.

## Policy, timeout, and cancellation

Run representative statements from every rejected write family. Each must fail
locally with exit code 2 and must not register an Athena query:

```bash
for sql in \
  'INSERT INTO target SELECT 1' \
  'UPDATE target SET value = 1' \
  'DELETE FROM target' \
  'MERGE INTO target USING source ON true WHEN MATCHED THEN DELETE' \
  'CREATE TABLE target (value integer)' \
  'ALTER TABLE target ADD COLUMNS (value integer)' \
  'DROP TABLE target' \
  "UNLOAD (SELECT 1) TO 's3://example.invalid/'" \
  'CALL system.example()' \
  'USE example'
do
  honk query \
    --connection "$HONK_CONNECTION" --session "$HONK_SESSION" \
    "$sql" >/dev/null
  test "$?" -eq 2 || break
done
```

For cancellation, start a known long-running read, wait long enough for Athena
to register it, then press `Ctrl-C`. Confirm exit code 130, capture the query ID
from the error, and confirm Athena records it as cancelled. For timeout, clone
the dev connection with a short `query_timeout`, run the same bounded test query,
and confirm exit code 4 plus cancellation in Athena. Restore the config afterward.

- [ ] Interrupt requests cancellation and exits 130.
- [ ] Timeout requests cancellation and exits 4.
- [ ] Honk reports the query ID in both cases once Athena has registered it.

## Staging and production

Use separate prepared profiles. Limit checks to session validation, discovery,
`SELECT 1`, and an approved partition-bounded read.

- [ ] Each session matches the configured account and role.
- [ ] Every command names its connection and session profile.
- [ ] Namespace selection and workgroup controls match the environment.
- [ ] No write reaches Athena and diagnostics reveal no credentials.

## Agent acceptance

Install or link `skills/honk` into the coding agent's skill directory. Start each
test in a context-free agent session so it must rely on the skill and task text.

Investigation prompt:

```text
Use Honk with connection dev and session profile dev-session to investigate
<question>. Read the dbt project at <path> for model context and lineage, then
confirm the deployed schema through Honk. Keep queries partition-bounded and
report the Athena query IDs you used. Use no other lakehouse access method.
```

Reviewed export prompt:

```text
Use Honk with connection dev and session profile dev-session to run the reviewed
SQL in <path>. Export CSV to <new-output-path>. Do not overwrite an existing
file. Report the Athena query ID and output path without printing the data.
```

- [ ] The investigation reads only relevant dbt source files, confirms the
      deployed schema through Honk, requests JSON Lines, bounds its queries, and
      reports connection, session, namespace, and query IDs.
- [ ] The export uses `--output`, writes the approved new path atomically, and
      does not expose rows in chat.
- [ ] An expired-session run stops and asks the user to refresh the profile. The
      agent does not inspect or modify AWS credentials.

## Final checks

```bash
credentials_checksum_after="$(shasum -a 256 "$HOME/.aws/credentials")"
test "$credentials_checksum_before" = "$credentials_checksum_after"

cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo deny check
```

- [ ] All checks pass.
- [ ] Known parser false rejections and their workarounds are documented.
- [ ] No credentials, tokens, private query results, or company identifiers were
      added to the repository.
