use std::fmt;
use std::future::Future;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::api::{
    ApiError, AthenaApi, QueryState, QueryStatistics, StartQuery, WorkGroup, WorkGroupState,
};
use super::{AthenaError, CancellationOutcome};
use crate::cli::OutputFormat;
use crate::config::Connection;
use crate::output::{Formatter, Rename, rename_columns};
use crate::policy::athena::{StatementKind, ValidatedQuery};

const INITIAL_POLL_DELAY: Duration = Duration::from_millis(100);
const MAX_POLL_DELAY: Duration = Duration::from_secs(5);
const CANCELLATION_TIMEOUT: Duration = Duration::from_secs(10);
const BYTE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

pub(super) struct QueryContext<'a> {
    pub(super) connection: &'a Connection,
    pub(super) query: &'a ValidatedQuery,
    pub(super) catalog: Option<&'a str>,
    pub(super) database: Option<&'a str>,
    pub(super) format: OutputFormat,
    pub(super) table_width: Option<usize>,
}

#[derive(Debug)]
pub(super) struct QueryReport {
    pub(super) query_id: String,
    pub(super) row_count: usize,
    pub(super) statistics: QueryStatistics,
    pub(super) renames: Vec<Rename>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Termination {
    Timeout,
    Interrupted,
}

trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TokioSleeper;

impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

pub(super) async fn execute<A: AthenaApi>(
    api: &A,
    context: QueryContext<'_>,
    output: &mut dyn Write,
) -> Result<QueryReport, AthenaError> {
    let timeout = context.connection.query_timeout;
    let termination = async move {
        tokio::select! {
            () = tokio::time::sleep(timeout) => Termination::Timeout,
            _ = tokio::signal::ctrl_c() => Termination::Interrupted,
        }
    };
    execute_with_termination(api, context, output, &TokioSleeper, termination).await
}

async fn execute_with_termination<A, S, F>(
    api: &A,
    context: QueryContext<'_>,
    output: &mut dyn Write,
    sleeper: &S,
    termination: F,
) -> Result<QueryReport, AthenaError>
where
    A: AthenaApi,
    S: Sleeper,
    F: Future<Output = Termination>,
{
    let tracker = QueryTracker::default();
    let timeout = context.connection.query_timeout;
    let selected = {
        let work = execute_inner(api, &context, output, sleeper, &tracker);
        tokio::pin!(work);
        tokio::pin!(termination);
        tokio::select! {
            result = &mut work => Selected::Completed(result),
            reason = &mut termination => Selected::Terminated(reason),
        }
    };

    match selected {
        Selected::Completed(Ok(report)) => Ok(report),
        Selected::Completed(Err(mut error)) => {
            let cancellation = cancel_running_query(api, &tracker).await;
            error.record_cancellation(cancellation);
            Err(error)
        }
        Selected::Terminated(reason) => {
            let query_id = tracker.query_id();
            let cancellation = cancel_running_query(api, &tracker).await;
            match reason {
                Termination::Timeout => Err(AthenaError::TimedOut {
                    query_id,
                    timeout,
                    cancellation,
                }),
                Termination::Interrupted => Err(AthenaError::Interrupted {
                    query_id,
                    cancellation,
                }),
            }
        }
    }
}

enum Selected {
    Completed(Result<QueryReport, AthenaError>),
    Terminated(Termination),
}

async fn execute_inner<A: AthenaApi, S: Sleeper>(
    api: &A,
    context: &QueryContext<'_>,
    output: &mut dyn Write,
    sleeper: &S,
    tracker: &QueryTracker,
) -> Result<QueryReport, AthenaError> {
    let connection = context.connection;
    let workgroup = api
        .get_work_group(&connection.workgroup)
        .await
        .map_err(|error| api_failure(error, Operation::GetWorkGroup, None))?;
    let output_location = submission_output_location(connection, &workgroup)?;

    let query_id = api
        .start_query(StartQuery {
            query: context.query,
            catalog: context.catalog,
            database: context.database,
            workgroup: &connection.workgroup,
            output_location,
        })
        .await
        .map_err(|error| api_failure(error, Operation::StartQueryExecution, None))?;
    if query_id.is_empty() {
        return Err(AthenaError::MalformedResponse {
            operation: Operation::StartQueryExecution,
            query_id: None,
            cancellation: CancellationOutcome::NotNeeded,
        });
    }
    tracker.started(&query_id);

    let mut poll_delay = INITIAL_POLL_DELAY;
    let statistics = loop {
        let status = api
            .get_query_status(&query_id)
            .await
            .map_err(|error| api_failure(error, Operation::GetQueryExecution, Some(&query_id)))?;
        match status.state {
            QueryState::Queued | QueryState::Running => {
                sleeper.sleep(poll_delay).await;
                poll_delay = poll_delay.saturating_mul(2).min(MAX_POLL_DELAY);
            }
            QueryState::Succeeded => {
                tracker.terminal();
                break status.statistics;
            }
            QueryState::Failed => {
                tracker.terminal();
                return Err(AthenaError::QueryFailed {
                    query_id,
                    reason: status.reason.map(|value| sanitize_reason(&value)),
                });
            }
            QueryState::Cancelled => {
                tracker.terminal();
                return Err(AthenaError::QueryCancelled { query_id });
            }
            QueryState::Unknown(state) => {
                return Err(AthenaError::UnknownQueryState {
                    query_id,
                    state,
                    cancellation: CancellationOutcome::NotNeeded,
                });
            }
        }
    };

    fetch_results(api, context, output, query_id, statistics).await
}

async fn fetch_results<A: AthenaApi>(
    api: &A,
    context: &QueryContext<'_>,
    output: &mut dyn Write,
    query_id: String,
    statistics: QueryStatistics,
) -> Result<QueryReport, AthenaError> {
    let mut page = api
        .get_result_page(&query_id, None)
        .await
        .map_err(|error| api_failure(error, Operation::GetQueryResults, Some(&query_id)))?;
    let first_columns = page.columns.clone();
    let (columns, renames) = rename_columns(&first_columns);
    let mut formatter = Formatter::begin(output, context.format, &columns, context.table_width)
        .map_err(|source| AthenaError::Output {
            query_id: query_id.clone(),
            source,
        })?;
    let mut first_page = true;
    loop {
        if page.columns != first_columns {
            return Err(AthenaError::MalformedResponse {
                operation: Operation::GetQueryResults,
                query_id: Some(query_id),
                cancellation: CancellationOutcome::NotNeeded,
            });
        }
        let skip = usize::from(first_page && context.query.kind() == StatementKind::Select);
        for row in page.rows.iter().skip(skip) {
            formatter.row(row).map_err(|source| AthenaError::Output {
                query_id: query_id.clone(),
                source,
            })?;
        }
        first_page = false;
        let Some(next_token) = page.next_token.take() else {
            break;
        };
        page = api
            .get_result_page(&query_id, Some(&next_token))
            .await
            .map_err(|error| api_failure(error, Operation::GetQueryResults, Some(&query_id)))?;
    }
    let row_count = formatter.finish().map_err(|source| AthenaError::Output {
        query_id: query_id.clone(),
        source,
    })?;
    Ok(QueryReport {
        query_id,
        row_count,
        statistics,
        renames,
    })
}

fn submission_output_location<'a>(
    connection: &'a Connection,
    workgroup: &WorkGroup,
) -> Result<Option<&'a str>, AthenaError> {
    match &workgroup.state {
        WorkGroupState::Enabled => {}
        WorkGroupState::Disabled => {
            return Err(AthenaError::WorkGroupDisabled {
                connection: connection.name.clone(),
                workgroup: connection.workgroup.clone(),
            });
        }
        WorkGroupState::Unknown(state) => {
            return Err(AthenaError::UnknownWorkGroupState {
                connection: connection.name.clone(),
                workgroup: connection.workgroup.clone(),
                state: state.clone(),
            });
        }
    }

    if workgroup.managed_results {
        return Ok(None);
    }
    if workgroup.enforces_configuration {
        if workgroup.output_location.is_some() {
            return Ok(None);
        }
        return Err(AthenaError::MissingResultLocation {
            connection: connection.name.clone(),
            workgroup: connection.workgroup.clone(),
        });
    }
    if let Some(location) = connection.output_location.as_deref() {
        return Ok(Some(location));
    }
    if workgroup.output_location.is_some() {
        return Ok(None);
    }
    Err(AthenaError::MissingResultLocation {
        connection: connection.name.clone(),
        workgroup: connection.workgroup.clone(),
    })
}

fn api_failure(error: ApiError, operation: Operation, query_id: Option<&str>) -> AthenaError {
    match error {
        ApiError::ExpiredCredentials => AthenaError::CredentialsExpired {
            operation,
            query_id: query_id.map(str::to_owned),
            cancellation: CancellationOutcome::NotNeeded,
        },
        ApiError::Unavailable => AthenaError::OperationFailed {
            operation,
            query_id: query_id.map(str::to_owned),
            cancellation: CancellationOutcome::NotNeeded,
        },
    }
}

fn sanitize_reason(reason: &str) -> String {
    const LIMIT: usize = 1_000;
    let mut cleaned = reason
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if cleaned.chars().count() > LIMIT {
        cleaned = cleaned.chars().take(LIMIT).collect();
        cleaned.push_str("...");
    }
    cleaned
}

pub(super) fn write_operations(
    writer: &mut dyn Write,
    connection: &Connection,
    session_name: &str,
    namespace: super::Namespace<'_>,
    report: &QueryReport,
    output_path: Option<&std::path::Path>,
    quiet: bool,
) -> std::io::Result<()> {
    if !quiet {
        writeln!(
            writer,
            "Connection: {}",
            sanitize_metadata(&connection.name)
        )?;
        writeln!(writer, "Session: {}", sanitize_metadata(session_name))?;
        if let Some(catalog) = namespace.catalog {
            writeln!(writer, "Catalog: {}", sanitize_metadata(catalog))?;
        }
        if let Some(database) = namespace.database {
            writeln!(writer, "Database: {}", sanitize_metadata(database))?;
        }
        writeln!(writer, "Query ID: {}", sanitize_metadata(&report.query_id))?;
        writeln!(writer, "Status: succeeded")?;
        writeln!(writer, "Rows: {}", report.row_count)?;
        writeln!(
            writer,
            "Elapsed: {}",
            format_milliseconds(report.statistics.total_execution_ms)
        )?;
        writeln!(
            writer,
            "Data scanned: {}",
            format_bytes(report.statistics.data_scanned_bytes)
        )?;
        if let Some(path) = output_path {
            writeln!(
                writer,
                "Output: {}",
                sanitize_metadata(&path.display().to_string())
            )?;
        }
    }
    for rename in &report.renames {
        let original = if rename.original.is_empty() {
            "<blank>"
        } else {
            &rename.original
        };
        writeln!(
            writer,
            "Warning: renamed output column {} to {}",
            sanitize_metadata(original),
            sanitize_metadata(&rename.output)
        )?;
    }
    Ok(())
}

fn sanitize_metadata(value: &str) -> String {
    crate::diagnostics::terminal_text(value)
}

fn format_milliseconds(value: Option<i64>) -> String {
    let Some(value) = value.filter(|value| *value >= 0) else {
        return "unknown".to_owned();
    };
    let seconds = value / 1_000;
    let tenths = value % 1_000 / 100;
    format!("{seconds}.{tenths}s")
}

fn format_bytes(value: Option<i64>) -> String {
    let Some(value) = value.filter(|value| *value >= 0) else {
        return "unknown".to_owned();
    };
    let value = u64::try_from(value).expect("non-negative i64 fits u64");
    let mut divisor = 1_u64;
    let mut unit = 0;
    while value / divisor >= 1_000 && unit < BYTE_UNITS.len() - 1 {
        divisor = divisor.saturating_mul(1_000);
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        let whole = value / divisor;
        let tenth = value % divisor * 10 / divisor;
        format!("{whole}.{tenth} {}", BYTE_UNITS[unit])
    }
}

#[derive(Clone, Default)]
struct QueryTracker(Arc<Mutex<QueryProgress>>);

#[derive(Default)]
struct QueryProgress {
    query_id: Option<String>,
    terminal: bool,
}

impl QueryTracker {
    fn started(&self, query_id: &str) {
        let mut progress = self.0.lock().expect("query tracker lock");
        progress.query_id = Some(query_id.to_owned());
    }

    fn terminal(&self) {
        self.0.lock().expect("query tracker lock").terminal = true;
    }

    fn running_query_id(&self) -> Option<String> {
        let progress = self.0.lock().expect("query tracker lock");
        (!progress.terminal)
            .then(|| progress.query_id.clone())
            .flatten()
    }

    fn query_id(&self) -> Option<String> {
        self.0.lock().expect("query tracker lock").query_id.clone()
    }
}

async fn cancel_running_query<A: AthenaApi>(
    api: &A,
    tracker: &QueryTracker,
) -> CancellationOutcome {
    let Some(query_id) = tracker.running_query_id() else {
        return CancellationOutcome::NotNeeded;
    };
    match tokio::time::timeout(CANCELLATION_TIMEOUT, api.stop_query(&query_id)).await {
        Ok(Ok(())) => CancellationOutcome::Requested,
        Ok(Err(_)) | Err(_) => CancellationOutcome::Failed,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    GetWorkGroup,
    StartQueryExecution,
    GetQueryExecution,
    GetQueryResults,
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::GetWorkGroup => "GetWorkGroup",
            Self::StartQueryExecution => "StartQueryExecution",
            Self::GetQueryExecution => "GetQueryExecution",
            Self::GetQueryResults => "GetQueryResults",
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::sync::Notify;

    use super::*;
    use crate::athena::api::ResultPage;
    use crate::config::Policy;
    use crate::output::ColumnSpec;

    struct MockApi {
        workgroup: WorkGroup,
        start_result: Mutex<Result<String, ApiError>>,
        statuses: Mutex<VecDeque<Result<super::super::api::QueryStatus, ApiError>>>,
        pages: Mutex<VecDeque<Result<ResultPage, ApiError>>>,
        calls: Mutex<Vec<String>>,
        start_requests: Mutex<Vec<RecordedStart>>,
        stop_result: Mutex<Result<(), ApiError>>,
        block_start: AtomicBool,
        start_started: Notify,
        block_poll: AtomicBool,
        poll_started: Notify,
        block_results: AtomicBool,
        results_started: Notify,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct RecordedStart {
        sql: String,
        catalog: Option<String>,
        database: Option<String>,
        workgroup: String,
        output_location: Option<String>,
    }

    impl MockApi {
        fn succeeding(states: impl IntoIterator<Item = QueryState>) -> Self {
            let statuses = states
                .into_iter()
                .map(|state| {
                    Ok(super::super::api::QueryStatus {
                        state,
                        reason: None,
                        statistics: QueryStatistics {
                            total_execution_ms: Some(1_800),
                            data_scanned_bytes: Some(23_100_000),
                        },
                    })
                })
                .collect();
            Self {
                workgroup: enabled_workgroup(),
                start_result: Mutex::new(Ok("query-123".to_owned())),
                statuses: Mutex::new(statuses),
                pages: Mutex::new(VecDeque::from([Ok(page())])),
                calls: Mutex::new(Vec::new()),
                start_requests: Mutex::new(Vec::new()),
                stop_result: Mutex::new(Ok(())),
                block_start: AtomicBool::new(false),
                start_started: Notify::new(),
                block_poll: AtomicBool::new(false),
                poll_started: Notify::new(),
                block_results: AtomicBool::new(false),
                results_started: Notify::new(),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl AthenaApi for MockApi {
        fn get_work_group<'a>(
            &'a self,
            name: &'a str,
        ) -> super::super::api::ApiFuture<'a, WorkGroup> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("workgroup:{name}"));
            let result = self.workgroup.clone();
            Box::pin(async move { Ok(result) })
        }

        fn start_query<'a>(
            &'a self,
            request: StartQuery<'a>,
        ) -> super::super::api::ApiFuture<'a, String> {
            self.calls
                .lock()
                .expect("calls lock")
                .push("start".to_owned());
            self.start_requests
                .lock()
                .expect("requests lock")
                .push(RecordedStart {
                    sql: request.query.sql().to_owned(),
                    catalog: request.catalog.map(str::to_owned),
                    database: request.database.map(str::to_owned),
                    workgroup: request.workgroup.to_owned(),
                    output_location: request.output_location.map(str::to_owned),
                });
            Box::pin(async move {
                if self.block_start.load(Ordering::SeqCst) {
                    self.start_started.notify_one();
                    std::future::pending().await
                } else {
                    self.start_result.lock().expect("start lock").clone()
                }
            })
        }

        fn get_query_status<'a>(
            &'a self,
            query_id: &'a str,
        ) -> super::super::api::ApiFuture<'a, super::super::api::QueryStatus> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("poll:{query_id}"));
            Box::pin(async move {
                if self.block_poll.load(Ordering::SeqCst) {
                    self.poll_started.notify_one();
                    std::future::pending().await
                } else {
                    self.statuses
                        .lock()
                        .expect("statuses lock")
                        .pop_front()
                        .unwrap_or(Err(ApiError::Unavailable))
                }
            })
        }

        fn stop_query<'a>(&'a self, query_id: &'a str) -> super::super::api::ApiFuture<'a, ()> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("stop:{query_id}"));
            let result = *self.stop_result.lock().expect("stop lock");
            Box::pin(async move { result })
        }

        fn get_result_page<'a>(
            &'a self,
            query_id: &'a str,
            next_token: Option<&'a str>,
        ) -> super::super::api::ApiFuture<'a, ResultPage> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(match next_token {
                    Some(token) => format!("results:{query_id}:{token}"),
                    None => format!("results:{query_id}"),
                });
            Box::pin(async move {
                if self.block_results.load(Ordering::SeqCst) {
                    self.results_started.notify_one();
                    std::future::pending().await
                } else {
                    self.pages
                        .lock()
                        .expect("pages lock")
                        .pop_front()
                        .unwrap_or(Err(ApiError::Unavailable))
                }
            })
        }
    }

    #[derive(Default)]
    struct RecordingSleeper(Mutex<Vec<Duration>>);

    impl Sleeper for RecordingSleeper {
        async fn sleep(&self, duration: Duration) {
            self.0.lock().expect("sleep lock").push(duration);
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "test failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn connection(output_location: Option<&str>) -> Connection {
        Connection {
            name: "dev".to_owned(),
            account: "111111111111".to_owned(),
            region: "eu-west-1".to_owned(),
            role_arn: "arn:aws:iam::111111111111:role/honk-readonly".to_owned(),
            workgroup: "analytics-dev".to_owned(),
            default_catalog: Some("AwsDataCatalog".to_owned()),
            default_database: Some("analytics_dev".to_owned()),
            output_location: output_location.map(str::to_owned),
            policy: Policy::ReadOnly,
            query_timeout: Duration::from_mins(30),
        }
    }

    fn enabled_workgroup() -> WorkGroup {
        WorkGroup {
            state: WorkGroupState::Enabled,
            output_location: Some("s3://workgroup-results/".to_owned()),
            managed_results: false,
            enforces_configuration: false,
        }
    }

    fn page() -> ResultPage {
        ResultPage {
            columns: vec![ColumnSpec {
                name: "answer".to_owned(),
                data_type: "integer".to_owned(),
            }],
            rows: vec![vec![Some("answer".to_owned())], vec![Some("42".to_owned())]],
            next_token: None,
        }
    }

    fn query() -> ValidatedQuery {
        ValidatedQuery::parse("SELECT 42 AS answer".to_owned()).expect("valid query")
    }

    async fn run_mock(
        api: &MockApi,
        connection: &Connection,
        query: &ValidatedQuery,
        sleeper: &RecordingSleeper,
    ) -> Result<(String, String), AthenaError> {
        let mut output = Vec::new();
        let mut operations = Vec::new();
        let report = execute_with_termination(
            api,
            QueryContext {
                connection,
                query,
                catalog: Some("AwsDataCatalog"),
                database: Some("analytics_dev"),
                format: OutputFormat::Table,
                table_width: Some(120),
            },
            &mut output,
            sleeper,
            std::future::pending(),
        )
        .await?;
        write_operations(
            &mut operations,
            connection,
            "dev-session",
            crate::athena::Namespace {
                catalog: Some("AwsDataCatalog"),
                database: Some("analytics_dev"),
            },
            &report,
            None,
            false,
        )
        .expect("metadata");
        Ok((
            String::from_utf8(output).expect("output UTF-8"),
            String::from_utf8(operations).expect("operations UTF-8"),
        ))
    }

    #[tokio::test]
    async fn submits_only_a_validated_query_and_polls_with_bounded_backoff() {
        let api = MockApi::succeeding([
            QueryState::Queued,
            QueryState::Running,
            QueryState::Succeeded,
        ]);
        let sleeper = RecordingSleeper::default();
        let query = query();
        let (output, operations) = run_mock(&api, &connection(None), &query, &sleeper)
            .await
            .expect("query succeeds");

        assert!(output.contains("| 42"));
        assert!(!output.contains("Query ID"));
        assert!(operations.contains("Query ID: query-123"));
        assert!(operations.contains("Rows: 1"));
        assert!(operations.contains("Elapsed: 1.8s"));
        assert!(operations.contains("Data scanned: 23.1 MB"));
        assert_eq!(
            sleeper.0.lock().expect("sleep lock").as_slice(),
            [Duration::from_millis(100), Duration::from_millis(200)]
        );
        assert_eq!(
            api.calls(),
            [
                "workgroup:analytics-dev",
                "start",
                "poll:query-123",
                "poll:query-123",
                "poll:query-123",
                "results:query-123",
            ]
        );
    }

    #[tokio::test]
    async fn query_submission_can_omit_catalog_and_database_context() {
        let api = MockApi::succeeding([QueryState::Succeeded]);
        let connection = connection(None);
        let query = query();
        let mut output = Vec::new();
        execute_with_termination(
            &api,
            QueryContext {
                connection: &connection,
                query: &query,
                catalog: None,
                database: None,
                format: OutputFormat::Table,
                table_width: Some(120),
            },
            &mut output,
            &RecordingSleeper::default(),
            std::future::pending(),
        )
        .await
        .expect("query without execution context");

        let request = &api.start_requests.lock().expect("requests lock")[0];
        assert_eq!(request.catalog, None);
        assert_eq!(request.database, None);
    }

    #[tokio::test]
    async fn pages_every_result_and_skips_select_header_only_once() {
        let api = MockApi::succeeding([QueryState::Succeeded]);
        let columns = vec![ColumnSpec {
            name: "n".to_owned(),
            data_type: "integer".to_owned(),
        }];
        *api.pages.lock().expect("pages lock") = VecDeque::from([
            Ok(ResultPage {
                columns: columns.clone(),
                rows: vec![vec![Some("n".into())], vec![Some("1".into())]],
                next_token: Some("page-2".into()),
            }),
            Ok(ResultPage {
                columns,
                rows: vec![vec![Some("2".into())], vec![Some("3".into())]],
                next_token: None,
            }),
        ]);

        let (output, operations) = run_mock(
            &api,
            &connection(None),
            &ValidatedQuery::parse("SELECT n FROM numbers".into()).expect("query"),
            &RecordingSleeper::default(),
        )
        .await
        .expect("query succeeds");

        for value in ["1", "2", "3"] {
            assert!(output.contains(value));
        }
        assert!(operations.contains("Rows: 3"));
        assert!(api.calls().contains(&"results:query-123:page-2".to_owned()));
    }

    #[tokio::test]
    async fn utility_result_keeps_its_first_row() {
        let api = MockApi::succeeding([QueryState::Succeeded]);
        *api.pages.lock().expect("pages lock") = VecDeque::from([Ok(ResultPage {
            columns: vec![ColumnSpec {
                name: "tab_name".into(),
                data_type: "varchar".into(),
            }],
            rows: vec![vec![Some("events".into())], vec![Some("users".into())]],
            next_token: None,
        })]);

        let (output, operations) = run_mock(
            &api,
            &connection(None),
            &ValidatedQuery::parse("SHOW TABLES".into()).expect("query"),
            &RecordingSleeper::default(),
        )
        .await
        .expect("query succeeds");

        assert!(output.contains("events"));
        assert!(output.contains("users"));
        assert!(operations.contains("Rows: 2"));
    }

    #[tokio::test]
    async fn duplicate_and_blank_columns_are_renamed_and_reported() {
        let api = MockApi::succeeding([QueryState::Succeeded]);
        *api.pages.lock().expect("pages lock") = VecDeque::from([Ok(ResultPage {
            columns: vec![
                ColumnSpec {
                    name: String::new(),
                    data_type: "varchar".into(),
                },
                ColumnSpec {
                    name: "value".into(),
                    data_type: "varchar".into(),
                },
                ColumnSpec {
                    name: "value".into(),
                    data_type: "varchar".into(),
                },
            ],
            rows: vec![
                vec![
                    Some(String::new()),
                    Some("value".into()),
                    Some("value".into()),
                ],
                vec![Some("a".into()), Some("b".into()), Some("c".into())],
            ],
            next_token: None,
        })]);

        let (output, operations) = run_mock(
            &api,
            &connection(None),
            &ValidatedQuery::parse("SELECT 1".into()).expect("query"),
            &RecordingSleeper::default(),
        )
        .await
        .expect("query succeeds");

        assert!(output.contains("_col1"));
        assert!(output.contains("value_2"));
        assert!(operations.contains("renamed output column <blank> to _col1"));
        assert!(operations.contains("renamed output column value to value_2"));
    }

    #[tokio::test]
    async fn polling_backoff_stops_growing_at_five_seconds() {
        let api = MockApi::succeeding([
            QueryState::Running,
            QueryState::Running,
            QueryState::Running,
            QueryState::Running,
            QueryState::Running,
            QueryState::Running,
            QueryState::Running,
            QueryState::Running,
            QueryState::Succeeded,
        ]);
        let sleeper = RecordingSleeper::default();
        run_mock(&api, &connection(None), &query(), &sleeper)
            .await
            .expect("query succeeds");
        assert_eq!(
            sleeper.0.lock().expect("sleep lock").as_slice(),
            [
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(400),
                Duration::from_millis(800),
                Duration::from_millis(1_600),
                Duration::from_millis(3_200),
                Duration::from_secs(5),
                Duration::from_secs(5),
            ]
        );
    }

    #[tokio::test]
    async fn workgroup_and_connection_choose_result_location_safely() {
        let query = query();
        for (workgroup, configured, expected) in [
            (
                enabled_workgroup(),
                Some("s3://connection/"),
                Some("s3://connection/"),
            ),
            (enabled_workgroup(), None, None),
            (
                WorkGroup {
                    enforces_configuration: true,
                    ..enabled_workgroup()
                },
                Some("s3://ignored/"),
                None,
            ),
            (
                WorkGroup {
                    output_location: None,
                    managed_results: true,
                    ..enabled_workgroup()
                },
                Some("s3://not-sent/"),
                None,
            ),
        ] {
            let mut api = MockApi::succeeding([QueryState::Succeeded]);
            api.workgroup = workgroup;
            run_mock(
                &api,
                &connection(configured),
                &query,
                &RecordingSleeper::default(),
            )
            .await
            .expect("query succeeds");
            assert_eq!(
                api.start_requests.lock().expect("requests lock")[0]
                    .output_location
                    .as_deref(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn missing_or_disabled_workgroup_configuration_fails_before_submit() {
        let query = query();
        let mut missing = MockApi::succeeding([QueryState::Succeeded]);
        missing.workgroup.output_location = None;
        let error = run_mock(
            &missing,
            &connection(None),
            &query,
            &RecordingSleeper::default(),
        )
        .await
        .expect_err("missing result location");
        assert!(matches!(error, AthenaError::MissingResultLocation { .. }));
        assert!(!missing.calls().contains(&"start".to_owned()));

        let mut disabled = MockApi::succeeding([QueryState::Succeeded]);
        disabled.workgroup.state = WorkGroupState::Disabled;
        let error = run_mock(
            &disabled,
            &connection(None),
            &query,
            &RecordingSleeper::default(),
        )
        .await
        .expect_err("disabled workgroup");
        assert!(matches!(error, AthenaError::WorkGroupDisabled { .. }));
        assert!(!disabled.calls().contains(&"start".to_owned()));
    }

    #[tokio::test]
    async fn submission_errors_have_no_query_to_cancel() {
        let api = MockApi::succeeding([]);
        *api.start_result.lock().expect("start lock") = Err(ApiError::Unavailable);
        let error = run_mock(
            &api,
            &connection(None),
            &query(),
            &RecordingSleeper::default(),
        )
        .await
        .expect_err("submission fails");
        assert!(matches!(
            error,
            AthenaError::OperationFailed {
                operation: Operation::StartQueryExecution,
                query_id: None,
                ..
            }
        ));
        assert!(!api.calls().iter().any(|call| call.starts_with("stop:")));
    }

    #[tokio::test]
    async fn missing_query_id_is_a_malformed_response_without_cancellation() {
        let api = MockApi::succeeding([]);
        *api.start_result.lock().expect("start lock") = Ok(String::new());
        let error = run_mock(
            &api,
            &connection(None),
            &query(),
            &RecordingSleeper::default(),
        )
        .await
        .expect_err("query ID is required");
        assert!(matches!(
            error,
            AthenaError::MalformedResponse {
                operation: Operation::StartQueryExecution,
                query_id: None,
                ..
            }
        ));
        assert!(!api.calls().iter().any(|call| call.starts_with("stop:")));
    }

    #[tokio::test]
    async fn remote_failure_reason_and_cancellation_are_terminal() {
        for (state, expected) in [
            (QueryState::Failed, "bad column"),
            (QueryState::Cancelled, "cancelled"),
        ] {
            let api = MockApi::succeeding([]);
            api.statuses.lock().expect("statuses lock").push_back(Ok(
                super::super::api::QueryStatus {
                    state,
                    reason: Some("bad column\n\u{1b}[31msecond line".to_owned()),
                    statistics: QueryStatistics::default(),
                },
            ));
            let error = run_mock(
                &api,
                &connection(None),
                &query(),
                &RecordingSleeper::default(),
            )
            .await
            .expect_err("terminal failure");
            assert!(error.to_string().contains(expected));
            assert!(!error.to_string().contains('\n'));
            assert!(!error.to_string().contains('\u{1b}'));
            assert!(!api.calls().iter().any(|call| call.starts_with("stop:")));
        }
    }

    #[tokio::test]
    async fn unknown_state_fails_closed_and_requests_cancellation() {
        let api = MockApi::succeeding([QueryState::Unknown("PAUSED".to_owned())]);
        let error = run_mock(
            &api,
            &connection(None),
            &query(),
            &RecordingSleeper::default(),
        )
        .await
        .expect_err("unknown state");
        assert!(matches!(error, AthenaError::UnknownQueryState { .. }));
        assert!(api.calls().contains(&"stop:query-123".to_owned()));
    }

    #[tokio::test]
    async fn timeout_and_interrupt_cancel_a_running_query() {
        for reason in [Termination::Timeout, Termination::Interrupted] {
            let api = MockApi::succeeding([]);
            api.block_poll.store(true, Ordering::SeqCst);
            let termination = async {
                api.poll_started.notified().await;
                reason
            };
            let mut output = Vec::new();
            let error = execute_with_termination(
                &api,
                QueryContext {
                    connection: &connection(None),
                    query: &query(),
                    catalog: Some("AwsDataCatalog"),
                    database: Some("analytics_dev"),
                    format: OutputFormat::Table,
                    table_width: Some(120),
                },
                &mut output,
                &RecordingSleeper::default(),
                termination,
            )
            .await
            .expect_err("terminated");
            assert_eq!(
                error.exit_code(),
                if reason == Termination::Interrupted {
                    130
                } else {
                    4
                }
            );
            assert!(error.to_string().contains("query-123"));
            assert!(error.to_string().contains("cancellation was requested"));
            assert!(api.calls().contains(&"stop:query-123".to_owned()));
        }
    }

    #[tokio::test]
    async fn timeout_and_interrupt_during_submission_have_no_query_to_cancel() {
        for reason in [Termination::Timeout, Termination::Interrupted] {
            let api = MockApi::succeeding([]);
            api.block_start.store(true, Ordering::SeqCst);
            let termination = async {
                api.start_started.notified().await;
                reason
            };
            let mut output = Vec::new();
            let error = execute_with_termination(
                &api,
                QueryContext {
                    connection: &connection(None),
                    query: &query(),
                    catalog: Some("AwsDataCatalog"),
                    database: Some("analytics_dev"),
                    format: OutputFormat::Table,
                    table_width: Some(120),
                },
                &mut output,
                &RecordingSleeper::default(),
                termination,
            )
            .await
            .expect_err("terminated during submission");
            assert_eq!(
                error.exit_code(),
                if reason == Termination::Interrupted {
                    130
                } else {
                    4
                }
            );
            assert!(!error.to_string().contains("query-123"));
            assert!(!api.calls().iter().any(|call| call.starts_with("stop:")));
        }
    }

    #[tokio::test]
    async fn timeout_and_interrupt_during_results_keep_succeeded_query_terminal() {
        for reason in [Termination::Timeout, Termination::Interrupted] {
            let api = MockApi::succeeding([QueryState::Succeeded]);
            api.block_results.store(true, Ordering::SeqCst);
            let termination = async {
                api.results_started.notified().await;
                reason
            };
            let mut output = Vec::new();
            let error = execute_with_termination(
                &api,
                QueryContext {
                    connection: &connection(None),
                    query: &query(),
                    catalog: Some("AwsDataCatalog"),
                    database: Some("analytics_dev"),
                    format: OutputFormat::Table,
                    table_width: Some(120),
                },
                &mut output,
                &RecordingSleeper::default(),
                termination,
            )
            .await
            .expect_err("terminated during result retrieval");
            assert_eq!(
                error.exit_code(),
                if reason == Termination::Interrupted {
                    130
                } else {
                    4
                }
            );
            assert!(error.to_string().contains("query-123"));
            assert!(!api.calls().iter().any(|call| call.starts_with("stop:")));
        }
    }

    #[tokio::test]
    async fn cancellation_failure_does_not_hide_expired_credentials() {
        let api = MockApi::succeeding([]);
        api.statuses
            .lock()
            .expect("statuses lock")
            .push_back(Err(ApiError::ExpiredCredentials));
        *api.stop_result.lock().expect("stop lock") = Err(ApiError::ExpiredCredentials);
        let error = run_mock(
            &api,
            &connection(None),
            &query(),
            &RecordingSleeper::default(),
        )
        .await
        .expect_err("credentials expired");
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("query-123"));
        assert!(
            error
                .to_string()
                .contains("cancellation request also failed")
        );
    }

    #[tokio::test]
    async fn result_fetch_failure_does_not_cancel_a_succeeded_query() {
        let api = MockApi::succeeding([QueryState::Succeeded]);
        *api.pages.lock().expect("pages lock") = VecDeque::from([Err(ApiError::Unavailable)]);
        let error = run_mock(
            &api,
            &connection(None),
            &query(),
            &RecordingSleeper::default(),
        )
        .await
        .expect_err("results fail");
        assert!(matches!(
            error,
            AthenaError::OperationFailed {
                operation: Operation::GetQueryResults,
                ..
            }
        ));
        assert!(!error.to_string().contains("cancellation"));
        assert!(!api.calls().iter().any(|call| call.starts_with("stop:")));
    }

    #[tokio::test]
    async fn output_failure_is_exit_five_after_query_success() {
        let api = MockApi::succeeding([QueryState::Succeeded]);
        let error = execute_with_termination(
            &api,
            QueryContext {
                connection: &connection(None),
                query: &query(),
                catalog: Some("AwsDataCatalog"),
                database: Some("analytics_dev"),
                format: OutputFormat::Table,
                table_width: Some(120),
            },
            &mut FailingWriter,
            &RecordingSleeper::default(),
            std::future::pending(),
        )
        .await
        .expect_err("output fails");
        assert_eq!(error.exit_code(), 5);
        assert!(error.to_string().contains("query-123"));
        assert!(!api.calls().iter().any(|call| call.starts_with("stop:")));
    }

    #[tokio::test]
    async fn failed_cancellation_does_not_hide_interrupt_exit_code() {
        let api = MockApi::succeeding([]);
        api.block_poll.store(true, Ordering::SeqCst);
        *api.stop_result.lock().expect("stop lock") = Err(ApiError::Unavailable);
        let termination = async {
            api.poll_started.notified().await;
            Termination::Interrupted
        };
        let mut output = Vec::new();
        let error = execute_with_termination(
            &api,
            QueryContext {
                connection: &connection(None),
                query: &query(),
                catalog: Some("AwsDataCatalog"),
                database: Some("analytics_dev"),
                format: OutputFormat::Table,
                table_width: Some(120),
            },
            &mut output,
            &RecordingSleeper::default(),
            termination,
        )
        .await
        .expect_err("interrupted");
        assert_eq!(error.exit_code(), 130);
        assert!(
            error
                .to_string()
                .contains("cancellation request also failed")
        );
    }

    #[tokio::test]
    async fn quiet_keeps_success_metadata_off_operational_output() {
        let api = MockApi::succeeding([QueryState::Succeeded]);
        let mut output = Vec::new();
        let mut operations = Vec::new();
        let report = execute_with_termination(
            &api,
            QueryContext {
                connection: &connection(None),
                query: &query(),
                catalog: Some("AwsDataCatalog"),
                database: Some("analytics_dev"),
                format: OutputFormat::Table,
                table_width: Some(120),
            },
            &mut output,
            &RecordingSleeper::default(),
            std::future::pending(),
        )
        .await
        .expect("query succeeds");
        write_operations(
            &mut operations,
            &connection(None),
            "dev-session",
            crate::athena::Namespace {
                catalog: Some("AwsDataCatalog"),
                database: Some("analytics_dev"),
            },
            &report,
            None,
            true,
        )
        .expect("metadata");
        assert!(!String::from_utf8_lossy(&output).contains("Query ID"));
        let operations = String::from_utf8(operations).expect("operations UTF-8");
        assert!(!operations.contains("Query ID"));
        assert!(operations.is_empty());
    }
}
