# Honk V1 implementation plan

Status: implementation in progress

Companion: [SPEC.md](./SPEC.md)

## 1. Outcome

V1 is a macOS Rust binary that lets a person or coding agent:

```bash
honk query --connection dev --format jsonl \
  --session dev-session \
  "SELECT order_id, status FROM analytics.orders LIMIT 20"
```

Honk loads an explicitly named temporary AWS session profile, verifies its AWS
identity, rejects anything outside its read-only SQL allowlist, executes through
the configured Athena workgroup, streams results, and leaves stdout safe for
machine consumption.

The implementation is complete when the first-party agent skill can conduct a
real investigation using a session prepared by the user, without reading or
changing AWS credentials directly.

## 2. Delivery milestones

| Milestone | Usable outcome |
| --- | --- |
| M0: policy risk closed | The Rust parser approach is proven |
| M1: first safe query | `honk query` runs one read query in dev with table output |
| M2: agent and export contract | All formats, file output, stable errors, timeout, and cancellation work |
| M3: discovery | Catalog, database, table, and column commands work without billed discovery SQL |
| M4: session-bounded V1 | Explicit session loading, agent skill, and manual environment verification are complete |

M0 is a gate for the SQL policy. The 1Password investigation is no longer a V1
gate; it begins after V1 as the first authentication fast follow.

M0 completed on 2026-08-26. The decision and evidence are in
[`docs/decisions/0001-sql-parser.md`](docs/decisions/0001-sql-parser.md).

## 3. Proposed project structure

Use one Cargo package and one binary. A workspace is unnecessary for V1.

```text
honk/
  Cargo.toml
  rust-toolchain.toml
  src/
    lib.rs
    main.rs
    cli.rs
    config.rs
    error.rs
    paths.rs
    sql_input.rs
    auth/
      mod.rs
      profile.rs
      identity.rs
    policy/
      mod.rs
      athena.rs
    athena/
      mod.rs
      api.rs
      query.rs
      sdk.rs
    metadata/
      mod.rs
      api.rs
      sdk.rs
      values.rs
    output/
      mod.rs
      columns.rs
      table.rs
      delimited.rs
      json.rs
      markdown.rs
  tests/
    cli.rs
    fixtures/
      sql/
      athena/
  skills/
    honk/
      SKILL.md
```

Keep AWS clients behind small internal traits where a state machine needs mocked
responses. Do not build a generic database-driver framework. Honk has one query
engine and one metadata system.

## 4. Initial dependency set

Choose the smallest set that supports the design:

- `clap` for argument parsing and generated help
- `tokio` for AWS calls, polling, signals, and timeouts
- `aws-config` for loading the explicitly selected session profile
- `aws-credential-types` and `aws-runtime` for opaque SDK credentials and an
  isolated shared-credentials file source
- `aws-sdk-sts` for `GetCallerIdentity`
- `aws-sdk-athena` for query execution and catalog fallback APIs
- `aws-sdk-glue` for Glue metadata
- `sqlparser = "=0.62.0"` with the `visitor` feature
- `serde`, `toml`, and `serde_json` for configuration and JSON output
- `thiserror` for typed internal errors
- A maintained CSV writer for CSV and TSV output
- A maintained table formatter that can write incrementally
- AWS Smithy mocks or narrow local mock traits for deterministic SDK tests

Do not add `aws-sdk-s3` in V1. Do not add DataFusion, Arrow, an embedded database,
or a Python runtime.

Run `cargo deny` before accepting the dependency set. Pin a supported Rust
toolchain after the AWS SDK versions resolve cleanly together.

## 5. Phase 0: SQL parser spike, completed

### 5.1 SQL parser spike

Goal: prove that Rust can accept the Athena read queries Honk needs while
rejecting every unclassified statement.

Completed work:

1. Created `spikes/sqlparser` with `sqlparser` 0.62.0.
2. Built a corpus from documented Athena queries and synthetic policy cases.
3. Tested `GenericDialect`, then implemented a narrow `AthenaDialect` because
   the generic dialect misclassified lambda arrows.
4. Implemented a recursive allowlist over the resulting AST.
5. Tested `EXPLAIN`, including rejection of `EXPLAIN ANALYZE` and rejected inner
   statements.
6. Tested multiple statements, comments, unusual semicolons, CTEs, nested queries,
   `UNNEST`, lambdas, arrays, maps, window expressions, and Iceberg read syntax.
7. Compared `sqlglot-rust` 0.10.29 and rejected it for policy use.

Results:

- 31 representative reads passed.
- 39 writes, administrative statements, and policy edge cases rejected.
- 20,000 generated cases covered arbitrary input and statement separation.
- The custom dialect is small enough to own in Honk.
- Athena Iceberg time travel is the only material gap. V1 rejects it until
  upstream supports the `FOR ... AS OF` grammar.

The spike made no AWS calls. Honk does not need Python for validation.

### 5.2 Phase output

The decision is recorded in `docs/decisions/0001-sql-parser.md`. The production
implementation should port the validator and corpus rather than depend on the
spike crate.

## 6. Phase 1: binary and strict configuration, completed

Goal: install `honk`, parse every command, and validate configuration without
contacting AWS.

Completed on 2026-09-01. The root Cargo package now provides the full V1 command
shape, strict local configuration loading, aggregate connection validation,
safe connection listing, typed Phase 1 errors, and exit code 2 for local
failures. Data, discovery, and session commands validate their local arguments
and selected connection, then return a clear not-implemented error until their
own phases land. The crate has no AWS or SQL-parser dependency yet, so this
milestone cannot contact AWS or submit SQL.

Verification at completion:

- 9 unit tests cover configuration parsing and validation.
- 14 process-level tests cover help, required flags, conflicts, safe output,
  configuration errors, and exit codes.
- `cargo fmt`, Clippy with warnings denied, and all tests pass.
- `cargo install --locked --path .` produces a working `honk 0.1.0` binary.

Work:

- Initialize the Cargo package and pin the Rust toolchain.
- Add `clap` command types for query, discovery, configuration, and session
  checking.
- Implement path resolution for the Honk configuration file.
- Deserialize TOML with unknown-field rejection.
- Model connections as typed structs.
- Parse human durations into checked values.
- Require explicit `--connection` and `--session` values on every data and
  session command.
- Require `policy = "read_only"` on every connection.
- Implement `honk connections` and `honk config check`.
- Implement typed errors and the agreed exit-code mapping.
- Validate session profile names as non-empty command inputs.

Tests:

- Valid three-environment configuration.
- Every missing required field.
- Unknown fields and policy values.
- Invalid query timeout, role ARN, region, and output location.
- No default connection or session behavior.
- `connections` never renders secret-bearing fields.
- CLI help, conflicts between SQL input forms, and exit codes.

Exit criteria:

- `cargo install --path .` installs a working `honk` command.
- `honk config check` reports all configuration errors without contacting AWS.
- No data command can run without both `--connection` and `--session`.

## 7. Phase 2: fail-closed SQL policy, completed

Goal: no SQL reaches an AWS client until Honk has classified it as one allowed
read statement.

Completed on 2026-09-01. `policy::athena` now owns the Athena dialect, recursive
AST allowlist, and `ValidatedQuery` type. The type stores the caller's exact SQL
and has a redacted `Debug` implementation. Parser and policy errors never echo
SQL tokens. The CLI reads inline SQL, files, and stdin through a bounded UTF-8
loader, then applies the policy before any future authentication or AWS code can
run. SQL input is limited to 1,048,576 bytes.

Verification at completion:

- The production corpus preserves all 31 accepted reads and 39 rejected policy
  cases from the spike.
- Three Athena Iceberg time-travel forms return the specific documented error.
- Two 10,000-case generated tests cover arbitrary UTF-8 and statement
  separation.
- Query-bodied writes, nested queries, `EXPLAIN ANALYZE`, unknown EXPLAIN
  options, excessive nesting, exact SQL retention, and diagnostic redaction have
  dedicated tests.
- 50 unit, policy, and process-level tests pass without AWS dependencies or
  network calls at runtime.

Work:

- Turn the Phase 0 parser spike into `policy::athena`.
- Return a typed classification such as `Select`, `Explain`, `Show`, or
  `Describe`.
- Require exactly one AST statement.
- Recursively validate every `Query`, CTE, set-operation branch, derived table,
  and subquery. Do not assume `Statement::Query` is read-only because its body
  can be `Insert`, `Update`, `Delete`, or `Merge`.
- Reject `SELECT INTO`, query locks, and unclassified query features.
- Reject both the boolean `EXPLAIN ANALYZE` field and an `ANALYZE` utility
  option.
- Retain the caller's original SQL in the validated-query type. Never submit
  AST-formatted SQL.
- Return a specific unsupported-syntax error for Athena Iceberg time travel.
- Give policy errors the statement kind and a short reason, without echoing
  sensitive SQL unnecessarily.
- Keep policy validation separate from result-shape classification.

Tests:

- Commit the complete allowed and rejected SQL corpus.
- Add regression fixtures for every parser bug discovered later.
- Verify comments and string literals cannot disguise a second statement.
- Verify `WITH` is allowed only when it represents an allowed query.
- Verify `EXPLAIN` cannot wrap a write.
- Verify parser errors and unknown AST variants reject.
- Verify the production corpus preserves the spike's 31 accepted reads, 39
  rejected cases, nesting check, and generated-input checks.

Exit criteria:

- The entire corpus produces the expected decision.
- Property or fuzz testing cannot make the validator panic.
- The production Athena submission function requires an already validated query
  type rather than a raw string alone.

## 8. Phase 3: explicit AWS session profiles, completed

Goal: load one user-prepared temporary AWS profile, verify that it is the
identity configured for the connection, and never mutate credential state.

Completed on 2026-09-01. Honk now reads only the explicitly named section from
`~/.aws/credentials`, requires temporary credentials, and freezes the checked
SDK credential object for the invocation. It does not consult
`~/.aws/config`, `AWS_PROFILE`, exported credentials, or the default SDK
credential chain. STS `GetCallerIdentity` verifies the configured account and
assumed-role name before any future Athena or Glue operation. Authentication
failures use exit code 3 and redact credential values.

The expiry safety margin is five minutes. Shared-credentials entries normally
provide no expiry metadata, so their validity at command start is established
by STS. `honk session check` uses this same path and prints only the selected
profile, connection, account, and role.

Verification at completion:

- 25 unit tests, 26 process-level CLI tests, and 18 policy tests pass.
- Mocked tests cover profile selection, missing and incomplete profiles, static
  credentials, expiry, STS expiry errors, account and role mismatches, ARN
  parsing, redaction, concurrent reads, and byte-for-byte file preservation.
- Process tests prove `AWS_PROFILE`, `AWS_SHARED_CREDENTIALS_FILE`, and exported
  credential variables cannot redirect the selected profile.
- `cargo fmt`, strict Clippy, and all 69 tests pass without live AWS access.
- `cargo deny` passes its advisory, ban, license, and source checks. The locked
  graph has four acknowledged duplicate-version warnings.
- Per the project constraint, the manual `dev-session` check was not run. It
  remains part of the pre-release manual verification checklist.

### 8.1 Internal interfaces

Use narrow interfaces so profile loading and identity checks are testable:

```text
SessionProfileLoader
  load(profile_name) -> temporary credentials provider

IdentityVerifier
  get_caller_identity(provider) -> account and ARN
```

Credential values must never enter ordinary application data structures when
the AWS SDK can retain them inside its provider. Any unavoidable secret-bearing
type must redact `Debug` and must not implement `Display`.

### 8.2 Profile loading and verification

Work:

- Make `--session <profile>` required for query and discovery commands.
- Construct an AWS profile credentials provider for exactly that profile. Do
  not let `AWS_PROFILE`, exported access keys, or the default SDK credential
  chain override the selected profile.
- Read the profile only from `~/.aws/credentials`; do not follow role chains or
  credential processes from `~/.aws/config` in V1.
- Require the resolved credentials to include a session token. Reject static
  long-lived credentials even if they would otherwise work.
- If the provider exposes credential expiry, reject a session that cannot cover
  the query timeout plus a safety margin. Treat absent expiry metadata as
  unknown rather than inventing a lifetime.
- Call STS `GetCallerIdentity` before Athena or Glue.
- Verify the returned account against the connection's `account`.
- Parse the assumed-role ARN and verify its role name against the connection's
  `role_arn`, ignoring the final role-session-name component.
- Classify missing, incomplete, expired, wrong-account, and wrong-role profiles
  as authentication/session errors with exit code 3.
- Implement `honk session check --connection <name> --session <profile>` using
  the same loading and verification path.
- Keep the AWS credentials and config files strictly read-only. Honk has no V1
  session state file.
- Redact credential values and session tokens from every error path.

Tests:

- The exact named profile is selected.
- Missing and incomplete profiles fail before lakehouse access.
- A profile without a session token is rejected.
- An expired token is classified correctly from a mocked STS response.
- Known near-expiry credentials are rejected; credentials with no expiry
  metadata proceed to identity verification.
- Wrong-account and wrong-role identities are rejected.
- Assumed-role ARNs with varied role session names compare correctly.
- Ambient `AWS_PROFILE` and exported credentials cannot redirect loading.
- Session checks and all error paths redact secrets.
- Parallel commands can read the same profile without writing shared state.

Exit criteria:

- Mocked providers cover the complete loading and identity-verification path.
- No automated test needs AWS.
- Honk leaves the credentials file byte-for-byte unchanged under concurrent
  checks.
- Before release, a manually prepared `dev-session` profile passes
  `honk session check`. This manual live check is intentionally deferred.

## 9. Phase 4: first safe Athena query, implementation completed

Goal: complete M1 with one dev query and table output.

Implemented on 2026-09-01. The production path builds Athena with the exact
credential object verified in Phase 3. It checks the configured workgroup,
submits only a `ValidatedQuery`, polls every 100 ms with exponential backoff
capped at five seconds, fetches the first result page, and renders an ASCII
table. Result reuse is disabled in each request.

The query tracker records the ID as soon as submission succeeds and marks the
query terminal before result retrieval. Timeout, interrupt, unknown state, and
poll failures request cancellation only while the query may still run. The
cancellation request has its own ten-second bound. Error messages preserve the
original failure and state whether cancellation was requested or failed.

Workgroup validation handles client output locations, enforced workgroup
locations, and Athena-managed results. A missing location or disabled workgroup
fails before submission. Successful metadata goes to stderr, while stdout
contains only the table. `--quiet` suppresses successful metadata.

Phase 4 intentionally supports only stdout table output and one result page.
Other formats and managed output files fail locally before authentication. A
next-page token produces an explicit warning. Phase 5 removes this temporary
limit by adding pagination and the complete output pipeline.

Verification at implementation completion:

- 41 unit tests, 27 process-level CLI tests, and 18 policy tests pass.
- Mocked tests cover submission, workgroups, result locations, queued and
  running polling, the five-second backoff cap, terminal states, malformed
  responses, timeout, interrupt, cancellation precedence, credential expiry,
  first-page results, output failures, quiet mode, and stdout separation.
- All 86 tests run without live AWS access.
- `cargo fmt` and strict Clippy pass.
- `cargo deny` passes advisory, ban, license, and source checks with the four
  already acknowledged duplicate-version warnings.
- `cargo install --locked --path .` produces a working `honk 0.1.0` release
  binary.
- Per the project constraint, the manual dev `SELECT 1` and long-query interrupt
  checks were not run. M1 remains unverified against dev until the manual
  release checklist.

Work:

- Build an Athena client from the explicitly selected session profile, not the
  ambient shell.
- Validate the configured workgroup with `GetWorkGroup`.
- Submit with catalog, database, workgroup, and configured result location when
  needed.
- Disable result reuse in V1 unless the workgroup overrides it.
- Poll `GetQueryExecution` with bounded exponential backoff.
- Capture query ID as soon as Athena returns it.
- Map queued, running, succeeded, failed, and cancelled states exhaustively.
- Enforce the configured timeout.
- On timeout, interrupt, or another post-submission local failure, call
  `StopQueryExecution` before returning when the query may still be running.
- Fetch the first `GetQueryResults` page and render a table.
- Print operational metadata on stderr only.

The query API must accept a validated policy object. There should be no internal
helper that accepts arbitrary SQL and submits it without validation.

Tests:

- Mocked submit success and service errors.
- Polling through queued and running states.
- Athena failure reason propagation.
- Unknown future SDK state handling.
- Timeout and remote cancellation.
- Interrupt and cancellation-failure precedence.
- Mid-query credential expiry reports the query ID even if cancellation also
  fails.
- Workgroup result-location behavior.
- Stdout contains only result data.

Exit criteria:

- Before release, a manually verified dev `SELECT 1` succeeds.
- A write statement fails locally before session loading or AWS access.
- Before release, interrupting a manual long-running dev query stops it in
  Athena.

## 10. Phase 5: paginated rows and every output format, implementation completed

Goal: complete M2 for agents and reviewed CSV exports.

Implemented on 2026-09-01. Honk now follows every Athena result token at the
1,000-row API maximum and formats each page before fetching the next. It skips
Athena's synthetic first row only for `SELECT`; `SHOW`, `DESCRIBE`, and other
utility results keep their first real row. Column metadata drives boolean,
integer, and floating-point JSON conversion. Decimal, date, timestamp, binary,
and complex values remain strings. Blank and duplicate names receive stable,
reported replacements.

All six output formats are implemented. Captured stdout defaults to JSON Lines,
while a terminal defaults to a width-aware table. The table uses fixed column
widths and ellipses when a horizontal result fits. A schema that cannot fit at
useful minimum widths switches to expanded records. This keeps formatting
streaming without collecting the complete result.

Managed output files infer their format from `.csv`, `.tsv`, `.json`, `.jsonl`,
`.ndjson`, `.md`, or `.markdown`. Explicit format conflicts and existing files
without `--force` fail locally. Honk writes a sibling temporary file, syncs it,
and renames it only after the formatter closes successfully. Failed and
interrupted exports leave no completed destination.

Verification at implementation completion:

- 55 unit tests, 29 process-level CLI tests, and 18 policy tests pass without
  live AWS access.
- Mocked query tests cover multiple pages, first-page header handling for reads
  and utilities, malformed metadata, output failures, renaming, quiet mode, and
  final row counts.
- Formatter tests cover every format, empty results, typed JSON values, CSV and
  TSV quoting, Markdown escaping, Unicode, wide tables, and interrupted JSON
  Lines output.
- File tests cover format inference, conflicts, overwrite protection, atomic
  replacement, and cleanup after an incomplete export.
- `cargo fmt`, strict Clippy, and `cargo deny` pass. The dependency graph keeps
  the four existing duplicate-version warnings.
- `cargo install --locked --path .` produces a working `honk 0.1.0` release
  binary.
- Manual dev checks for pagination, every format, and atomic file export remain
  in the release checklist rather than the automated suite.

### 10.1 Result pipeline

Work:

- Page `GetQueryResults` at Athena's maximum supported page size.
- Read column metadata once and construct unique output names.
- Handle Athena's first-page header-row behavior according to validated statement
  class.
- Convert values using declared Athena types.
- Pass rows through one formatter interface without accumulating the full result.
- Count emitted rows and retain final query statistics.
- Handle a broken stdout pipe without a panic or secret-bearing diagnostic.

### 10.2 Formatters

Implement:

- Human table
- CSV
- TSV
- JSON array of objects
- JSON Lines objects
- Markdown table

All formatters use the same renamed columns and typed values. JSON array output
must stream its opening delimiter, row separators, and closing delimiter without
collecting rows.

### 10.3 File output

Work:

- Select a format from a recognized extension when `--format` is absent.
- Reject explicit format and extension conflicts.
- Refuse existing destinations unless `--force` is supplied.
- Write to a temporary file in the destination directory.
- Rename only after the closing formatter write succeeds.
- Remove temporary output after failures where possible.

Tests:

- Multiple result pages and an empty result.
- Header-row handling for select and utility statements.
- Duplicate and blank column names.
- NULL, boolean, numeric, decimal, text, date, timestamp, and complex values.
- Formatter snapshots containing quotes, delimiters, newlines, and Unicode.
- Interrupted JSON versus independently valid JSON Lines.
- TTY-aware default selection.
- `--quiet` behavior.
- Atomic output, overwrite protection, and format conflicts.
- Exact stderr metadata and exit codes.

Exit criteria:

- A multi-page mocked result never grows memory with total row count.
- Piped output defaults to JSON Lines and direct terminal output defaults to a
  table.
- An interrupted file export never appears as a completed destination.

## 11. Phase 6: catalog discovery

Goal: complete M3 without running discovery SQL.

Implemented on 2026-09-03. Honk now uses a metadata-only API boundary with no
query-submission method. `AwsDataCatalog` databases, tables, and descriptions
use Glue; other catalogs use Athena's metadata APIs. All four discovery
commands paginate, normalize into stable rows, and share the query output and
atomic-file pipeline.

Work:

- Implement `catalogs` with Athena `ListDataCatalogs`.
- Route `AwsDataCatalog` discovery through Glue.
- Implement paginated Glue database, table, and table-detail calls.
- Use Athena metadata APIs for non-Glue and federated catalogs.
- Normalize both providers into shared catalog, database, table, and column row
  types.
- Include partition keys, locations, table parameters, view markers, and Iceberg
  markers when the provider returns them.
- Apply the same format and file-output pipeline as queries.
- Define actionable errors for missing objects and unavailable federated
  catalogs.

Do not reconstruct full DDL in V1. `describe` returns structured metadata rather
than a guessed `CREATE TABLE` statement.

Tests:

- Athena and Glue pagination.
- Empty catalogs and databases.
- Tables, views, partitions, and Iceberg markers.
- Non-Glue catalog fallback.
- Missing database and table errors.
- Stable JSON Lines field names for every metadata command.
- No path invokes `StartQueryExecution`.

Exit criteria:

- An agent can discover an unfamiliar table and its columns without SQL.
- Manual dev discovery produces no Athena query execution.

## 12. Phase 7: hardening and contract tests

Goal: make failures predictable before exposing Honk to autonomous use.

Work:

- Add process-level CLI tests for every command.
- Audit all logs and errors for secret leakage.
- Add panic hooks that redact secret-bearing state.
- Test malformed AWS responses and absent optional fields.
- Test simultaneous commands using the same read-only session profile.
- Test timeout behavior while submitting, polling, and retrieving pages.
- Verify `Ctrl-C` behavior in each phase.
- Run `cargo fmt`, `clippy` with warnings denied for Honk code, `cargo test`,
  `cargo deny`, and an unused-dependency check.
- Document configuration and setup without embedding company account values in
  tracked examples.

Exit criteria:

- Automated tests use no real AWS account.
- Every failure category maps to the specified exit code.
- A captured stdout stream never contains progress or authentication text.
- A repository secret scan finds no test credentials or tokens.

## 13. Phase 8: agent skill and manual release verification

Goal: complete M4 with the actual user workflow.

### 13.1 First-party skill

Write `skills/honk/SKILL.md` after the CLI contract stops changing. It should
teach an agent to:

- Check configuration and session status when needed.
- Name every connection explicitly.
- Name the user-supplied session profile explicitly.
- Discover metadata before writing unfamiliar SQL.
- Request JSON Lines explicitly.
- Use bounded queries and inspect intermediate results.
- Use atomic file output for exports.
- Read operational context from stderr.
- Report Athena query IDs.
- Never inspect, create, refresh, or remove AWS credentials.
- Ask the user for a refreshed session if Honk reports expiry.

Test the skill with realistic investigation prompts and a context-free agent.
The setup guide must explain that the user prepares the AWS profile first and
supplies both the connection and session names with the task. It must not imply
that Honk renews the session in V1.

### 13.2 Manual verification checklist

Dev:

- Prepared temporary session accepted
- Missing, static, and expired sessions rejected
- Wrong-account and wrong-role sessions rejected
- AWS files unchanged after all commands
- Catalog and schema discovery
- Simple, CTE, join, aggregate, window, and Iceberg read queries
- Multi-page results
- Every output format
- Atomic CSV export
- Policy rejection for every write family
- Timeout and cancellation visible in Athena
- Bytes-scanned reporting

Staging and production:

- Correct account, role, region, workgroup, catalog, and database
- Explicit temporary session profiles
- Read queries only
- Workgroup scan controls
- No credential leakage

Agent workflow:

- One session-bounded, unsupervised investigation using only Honk
- One reviewed SQL-to-CSV request
- Correct connection, session profile, and query IDs in the final report

Exit criteria:

- Every release gate in the spec is satisfied.
- Known Athena syntax false rejections are documented with workarounds or fixed.
- The user can leave an agent to complete an investigation without an MFA prompt
  while the prepared session remains valid.

## 14. What to take from Arris

Use the Arris investigation as evidence and a source of test cases.

Take:

- The official Rust AWS SDK approach.
- Free `GetWorkGroup` connection validation.
- Submit, poll, terminal-state, and cancellation behavior.
- The 100 ms to 5 second polling backoff.
- Athena header-row test cases.
- Type-driven conversion and decimal preservation.
- SDK-level mock testing.
- The discovery that Rust AWS profiles do not handle `mfa_serial` prompts.

Leave:

- The generic `DatabaseDriver` abstraction.
- Frontend and row-editing integration.
- Raw SQL mutation support.
- Keyword-based result classification.
- Direct S3 result download and the custom CSV scanner.
- DDL reconstruction.
- DataFusion and federation integration.
- dbt integration.

Reimplement the required behavior inside Honk. Do not create a dependency on the
Arris project.

## 15. Suggested commit sequence

Keep commits independently testable:

1. `docs: record the parser spike outcome`
2. `build: scaffold the honk rust cli`
3. `feat: add strict connection configuration`
4. `feat: enforce the athena read-only policy`
5. `feat: load and verify explicit aws sessions`
6. `feat: execute and cancel athena queries`
7. `feat: stream paginated query results`
8. `feat: add machine and terminal output formats`
9. `feat: write result files atomically`
10. `feat: add athena and glue discovery commands`
11. `test: harden cli and secret handling contracts`
12. `docs: add setup guide and honk agent skill`

Do not combine the parser policy and AWS submission in one initial commit. The
boundary between them is the most important invariant in the project and should
be reviewable on its own.

## 16. Post-V1 fast follow: Honk-managed MFA sessions

Begin this phase only after V1 is implemented and usable with explicit session
profiles. V1 remains supported as the manual-session path even if this phase
succeeds.

### 16.1 1Password spike

Goal: determine whether Honk can obtain the current MFA TOTP and refresh its own
AWS session without user interaction.

This spike is user-led because it depends on the available 1Password account and
company administrative settings.

Work:

1. Install and evaluate the supported `op` CLI workflow.
2. Determine whether the account supports service accounts.
3. Create a dedicated test vault and MFA test item if permitted.
4. Test whether a read-only service account can be scoped to only that vault.
5. Verify how a non-interactive process retrieves the current TOTP.
6. Test from a fresh shell, after restart, and while the desktop app is locked.
7. Decide how the service-account token is supplied and stored locally.
8. Confirm that command lines, process listings, errors, and logs expose neither
   the token nor the TOTP.
9. Document failure behavior and the viable alternatives if service accounts
   are unavailable.

The spike succeeds when an unattended local process can retrieve only the
intended MFA item through a documented 1Password mechanism. Record the decision
under `docs/decisions/` before implementing it.

### 16.2 Conditional implementation

If the spike succeeds, design Honk-owned profiles as an additional, explicit
session source. Add source-profile loading, STS `AssumeRole`, session duration
and refresh-margin configuration, atomic credential storage, concurrent refresh
locking, and `auth status`, `auth refresh`, and `auth logout`. Preserve the V1
invariant that every data command names its session source and every resulting
identity is checked against the selected connection.

Do not silently reinterpret a user-owned `--session` profile as Honk-owned or
modify it during refresh. The CLI design must make ownership unambiguous before
implementation begins.

## 17. Deferred implementation

Do not add these while building V1:

- S3-direct result fetching
- Interactive shell
- Default connections
- Pager and row display limits
- Unrestricted or write policies
- Credential export
- Local audit logs
- Automated live-account tests
- DDL reconstruction
- General database abstractions
- Cross-platform release automation

Each deferred item needs evidence from actual Honk usage before it enters the
plan.
