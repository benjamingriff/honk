use std::future::Future;
use std::pin::Pin;

use crate::output::ColumnSpec;
use crate::policy::athena::ValidatedQuery;

pub(super) type ApiFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApiError {
    ExpiredCredentials,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkGroup {
    pub(super) state: WorkGroupState,
    pub(super) output_location: Option<String>,
    pub(super) managed_results: bool,
    pub(super) enforces_configuration: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WorkGroupState {
    Enabled,
    Disabled,
    Unknown(String),
}

pub(super) struct StartQuery<'a> {
    pub(super) query: &'a ValidatedQuery,
    pub(super) catalog: &'a str,
    pub(super) database: &'a str,
    pub(super) workgroup: &'a str,
    pub(super) output_location: Option<&'a str>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct QueryStatistics {
    pub(super) total_execution_ms: Option<i64>,
    pub(super) data_scanned_bytes: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct QueryStatus {
    pub(super) state: QueryState,
    pub(super) reason: Option<String>,
    pub(super) statistics: QueryStatistics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum QueryState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResultPage {
    pub(crate) columns: Vec<ColumnSpec>,
    pub(crate) rows: Vec<Vec<Option<String>>>,
    pub(crate) next_token: Option<String>,
}

pub(super) trait AthenaApi: Send + Sync {
    fn get_work_group<'a>(&'a self, name: &'a str) -> ApiFuture<'a, WorkGroup>;

    fn start_query<'a>(&'a self, request: StartQuery<'a>) -> ApiFuture<'a, String>;

    fn get_query_status<'a>(&'a self, query_id: &'a str) -> ApiFuture<'a, QueryStatus>;

    fn stop_query<'a>(&'a self, query_id: &'a str) -> ApiFuture<'a, ()>;

    fn get_result_page<'a>(
        &'a self,
        query_id: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, ResultPage>;
}
