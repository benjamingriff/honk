use aws_config::BehaviorVersion;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_athena::config::Region;
use aws_sdk_athena::error::ProvideErrorMetadata as _;
use aws_sdk_athena::types::{
    QueryExecutionContext, QueryExecutionState, ResultConfiguration, ResultReuseByAgeConfiguration,
    ResultReuseConfiguration,
};

use super::api::{
    ApiError, ApiFuture, AthenaApi, QueryState, QueryStatistics, QueryStatus, ResultPage,
    StartQuery, WorkGroup, WorkGroupState,
};

#[derive(Clone, Debug)]
pub(crate) struct SdkAthena {
    client: aws_sdk_athena::Client,
}

impl SdkAthena {
    pub(crate) async fn new(provider: SharedCredentialsProvider, region: &str) -> Self {
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .empty_test_environment()
            .region(Region::new(region.to_owned()))
            .credentials_provider(provider)
            .load()
            .await;
        Self {
            client: aws_sdk_athena::Client::new(&sdk_config),
        }
    }
}

impl AthenaApi for SdkAthena {
    fn get_work_group<'a>(&'a self, name: &'a str) -> ApiFuture<'a, WorkGroup> {
        Box::pin(async move {
            let response = self
                .client
                .get_work_group()
                .work_group(name)
                .send()
                .await
                .map_err(|error| classify_error(error.as_service_error().and_then(|e| e.code())))?;
            let workgroup = response.work_group().ok_or(ApiError::Unavailable)?;
            let state = match workgroup.state() {
                Some(aws_sdk_athena::types::WorkGroupState::Enabled) => WorkGroupState::Enabled,
                Some(aws_sdk_athena::types::WorkGroupState::Disabled) => WorkGroupState::Disabled,
                Some(other) => WorkGroupState::Unknown(other.as_str().to_owned()),
                None => return Err(ApiError::Unavailable),
            };
            let configuration = workgroup.configuration();
            let output_location = configuration
                .and_then(|value| value.result_configuration())
                .and_then(|value| value.output_location())
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let managed_results = configuration
                .and_then(|value| value.managed_query_results_configuration())
                .is_some_and(aws_sdk_athena::types::ManagedQueryResultsConfiguration::enabled);
            let enforces_configuration = configuration
                .and_then(
                    aws_sdk_athena::types::WorkGroupConfiguration::enforce_work_group_configuration,
                )
                .unwrap_or(false);
            Ok(WorkGroup {
                state,
                output_location,
                managed_results,
                enforces_configuration,
            })
        })
    }

    fn start_query<'a>(&'a self, request: StartQuery<'a>) -> ApiFuture<'a, String> {
        Box::pin(async move {
            let context = QueryExecutionContext::builder()
                .catalog(request.catalog)
                .database(request.database)
                .build();
            let reuse = ResultReuseConfiguration::builder()
                .result_reuse_by_age_configuration(
                    ResultReuseByAgeConfiguration::builder()
                        .enabled(false)
                        .build(),
                )
                .build();
            let result_configuration = request.output_location.map(|location| {
                ResultConfiguration::builder()
                    .output_location(location)
                    .build()
            });
            let response = self
                .client
                .start_query_execution()
                .query_string(request.query.sql())
                .query_execution_context(context)
                .work_group(request.workgroup)
                .result_reuse_configuration(reuse)
                .set_result_configuration(result_configuration)
                .send()
                .await
                .map_err(|error| classify_error(error.as_service_error().and_then(|e| e.code())))?;
            response
                .query_execution_id()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(ApiError::Unavailable)
        })
    }

    fn get_query_status<'a>(&'a self, query_id: &'a str) -> ApiFuture<'a, QueryStatus> {
        Box::pin(async move {
            let response = self
                .client
                .get_query_execution()
                .query_execution_id(query_id)
                .send()
                .await
                .map_err(|error| classify_error(error.as_service_error().and_then(|e| e.code())))?;
            let execution = response.query_execution().ok_or(ApiError::Unavailable)?;
            let status = execution.status().ok_or(ApiError::Unavailable)?;
            let state = match status.state().ok_or(ApiError::Unavailable)? {
                QueryExecutionState::Queued => QueryState::Queued,
                QueryExecutionState::Running => QueryState::Running,
                QueryExecutionState::Succeeded => QueryState::Succeeded,
                QueryExecutionState::Failed => QueryState::Failed,
                QueryExecutionState::Cancelled => QueryState::Cancelled,
                other => QueryState::Unknown(other.as_str().to_owned()),
            };
            let statistics = execution.statistics();
            Ok(QueryStatus {
                state,
                reason: status.state_change_reason().map(str::to_owned),
                statistics: QueryStatistics {
                    total_execution_ms: statistics
                        .and_then(aws_sdk_athena::types::QueryExecutionStatistics::total_execution_time_in_millis),
                    data_scanned_bytes: statistics.and_then(
                        aws_sdk_athena::types::QueryExecutionStatistics::data_scanned_in_bytes,
                    ),
                },
            })
        })
    }

    fn stop_query<'a>(&'a self, query_id: &'a str) -> ApiFuture<'a, ()> {
        Box::pin(async move {
            self.client
                .stop_query_execution()
                .query_execution_id(query_id)
                .send()
                .await
                .map_err(|error| classify_error(error.as_service_error().and_then(|e| e.code())))?;
            Ok(())
        })
    }

    fn get_result_page<'a>(
        &'a self,
        query_id: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, ResultPage> {
        Box::pin(async move {
            let response = self
                .client
                .get_query_results()
                .query_execution_id(query_id)
                .max_results(1_000)
                .set_next_token(next_token.map(str::to_owned))
                .send()
                .await
                .map_err(|error| classify_error(error.as_service_error().and_then(|e| e.code())))?;
            let result_set = response.result_set().ok_or(ApiError::Unavailable)?;
            let columns = result_set
                .result_set_metadata()
                .ok_or(ApiError::Unavailable)?
                .column_info()
                .iter()
                .map(|column| super::api::ResultColumn {
                    name: column
                        .label()
                        .filter(|label| !label.is_empty())
                        .unwrap_or_else(|| column.name())
                        .to_owned(),
                    data_type: column.r#type().to_owned(),
                })
                .collect();
            let rows = result_set
                .rows()
                .iter()
                .map(|row| {
                    row.data()
                        .iter()
                        .map(|datum| datum.var_char_value().map(str::to_owned))
                        .collect()
                })
                .collect();
            Ok(ResultPage {
                columns,
                rows,
                next_token: response.next_token().map(str::to_owned),
            })
        })
    }
}

fn classify_error(code: Option<&str>) -> ApiError {
    if crate::auth::is_expired_aws_error_code(code) {
        ApiError::ExpiredCredentials
    } else {
        ApiError::Unavailable
    }
}
