pub(crate) mod api;
mod query;
mod sdk;

pub use query::Operation;

use std::error::Error;
use std::fmt;
use std::io;
use std::time::Duration;

use crate::auth::VerifiedSession;
use crate::config::Connection;
use crate::output::OutputPlan;
use crate::policy::athena::ValidatedQuery;

#[derive(Clone, Copy)]
pub(crate) struct Namespace<'a> {
    pub catalog: Option<&'a str>,
    pub database: Option<&'a str>,
}

pub(crate) async fn run_query(
    connection: &Connection,
    session_name: &str,
    session: &VerifiedSession,
    query: &ValidatedQuery,
    namespace: Namespace<'_>,
    output_plan: OutputPlan,
    quiet: bool,
) -> Result<(), AthenaError> {
    let mut output = output_plan
        .prepare()
        .map_err(|source| AthenaError::PrepareOutput { source })?;
    let client = sdk::SdkAthena::new(session.credentials_provider(), &connection.region).await;
    let stderr = io::stderr();
    let format = output.format;
    let table_width = output.table_width;
    let report = query::execute(
        &client,
        query::QueryContext {
            connection,
            query,
            catalog: namespace.catalog,
            database: namespace.database,
            format,
            table_width,
        },
        output.writer(),
    )
    .await?;
    let query_id = report.query_id.clone();
    let output_path = output.commit().map_err(|source| AthenaError::Output {
        query_id: query_id.clone(),
        source,
    })?;
    query::write_operations(
        &mut stderr.lock(),
        connection,
        session_name,
        namespace,
        &report,
        output_path.as_deref(),
        quiet,
    )
    .map_err(|source| AthenaError::Output { query_id, source })?;
    Ok(())
}

#[derive(Debug)]
pub enum AthenaError {
    WorkGroupDisabled {
        connection: String,
        workgroup: String,
    },
    UnknownWorkGroupState {
        connection: String,
        workgroup: String,
        state: String,
    },
    MissingResultLocation {
        connection: String,
        workgroup: String,
    },
    CredentialsExpired {
        operation: Operation,
        query_id: Option<String>,
        cancellation: CancellationOutcome,
    },
    OperationFailed {
        operation: Operation,
        query_id: Option<String>,
        cancellation: CancellationOutcome,
    },
    MalformedResponse {
        operation: Operation,
        query_id: Option<String>,
        cancellation: CancellationOutcome,
    },
    QueryFailed {
        query_id: String,
        reason: Option<String>,
    },
    QueryCancelled {
        query_id: String,
    },
    UnknownQueryState {
        query_id: String,
        state: String,
        cancellation: CancellationOutcome,
    },
    TimedOut {
        query_id: Option<String>,
        timeout: Duration,
        cancellation: CancellationOutcome,
    },
    Interrupted {
        query_id: Option<String>,
        cancellation: CancellationOutcome,
    },
    PrepareOutput {
        source: io::Error,
    },
    Output {
        query_id: String,
        source: io::Error,
    },
}

impl AthenaError {
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::CredentialsExpired { .. } => 3,
            Self::PrepareOutput { .. } | Self::Output { .. } => 5,
            Self::Interrupted { .. } => 130,
            _ => 4,
        }
    }

    fn record_cancellation(&mut self, outcome: CancellationOutcome) {
        match self {
            Self::CredentialsExpired { cancellation, .. }
            | Self::OperationFailed { cancellation, .. }
            | Self::MalformedResponse { cancellation, .. }
            | Self::UnknownQueryState { cancellation, .. }
            | Self::TimedOut { cancellation, .. }
            | Self::Interrupted { cancellation, .. } => *cancellation = outcome,
            Self::WorkGroupDisabled { .. }
            | Self::UnknownWorkGroupState { .. }
            | Self::MissingResultLocation { .. }
            | Self::QueryFailed { .. }
            | Self::QueryCancelled { .. }
            | Self::PrepareOutput { .. }
            | Self::Output { .. } => {}
        }
    }
}

impl fmt::Display for AthenaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkGroupDisabled {
                connection,
                workgroup,
            } => write!(
                formatter,
                "Athena workgroup {workgroup:?} for connection {connection:?} is disabled"
            ),
            Self::UnknownWorkGroupState {
                connection,
                workgroup,
                state,
            } => write!(
                formatter,
                "Athena workgroup {workgroup:?} for connection {connection:?} returned unknown state {state:?}"
            ),
            Self::MissingResultLocation {
                connection,
                workgroup,
            } => write!(
                formatter,
                "connection {connection:?} and Athena workgroup {workgroup:?} provide no query result location"
            ),
            Self::CredentialsExpired {
                operation,
                query_id,
                cancellation,
            } => {
                write!(
                    formatter,
                    "AWS credentials expired during Athena {operation}"
                )?;
                write_query_context(formatter, query_id.as_deref(), *cancellation)
            }
            Self::OperationFailed {
                operation,
                query_id,
                cancellation,
            } => {
                write!(formatter, "Athena {operation} failed")?;
                write_query_context(formatter, query_id.as_deref(), *cancellation)
            }
            Self::MalformedResponse {
                operation,
                query_id,
                cancellation,
            } => {
                write!(
                    formatter,
                    "Athena {operation} returned an incomplete response"
                )?;
                write_query_context(formatter, query_id.as_deref(), *cancellation)
            }
            Self::QueryFailed { query_id, reason } => {
                write!(formatter, "Athena query {query_id:?} failed")?;
                if let Some(reason) = reason {
                    write!(formatter, ": {reason}")?;
                }
                Ok(())
            }
            Self::QueryCancelled { query_id } => {
                write!(formatter, "Athena query {query_id:?} was cancelled")
            }
            Self::UnknownQueryState {
                query_id,
                state,
                cancellation,
            } => {
                write!(
                    formatter,
                    "Athena query {query_id:?} returned unknown state {state:?}"
                )?;
                write_cancellation_status(formatter, *cancellation)
            }
            Self::TimedOut {
                query_id,
                timeout,
                cancellation,
            } => {
                write!(
                    formatter,
                    "Athena command exceeded its configured timeout of {}",
                    humantime::format_duration(*timeout)
                )?;
                write_query_context(formatter, query_id.as_deref(), *cancellation)
            }
            Self::Interrupted {
                query_id,
                cancellation,
            } => {
                formatter.write_str("Athena command was interrupted")?;
                write_query_context(formatter, query_id.as_deref(), *cancellation)
            }
            Self::PrepareOutput { .. } | Self::Output { .. } => write_output_error(formatter, self),
        }
    }
}

fn write_output_error(formatter: &mut fmt::Formatter<'_>, error: &AthenaError) -> fmt::Result {
    match error {
        AthenaError::PrepareOutput { source } => {
            write!(formatter, "cannot prepare result output: {source}")
        }
        AthenaError::Output { query_id, source } => write!(
            formatter,
            "cannot write output for Athena query {query_id:?}: {source}"
        ),
        _ => unreachable!("output formatter only receives output errors"),
    }
}

impl Error for AthenaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PrepareOutput { source } | Self::Output { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn write_query_id(formatter: &mut fmt::Formatter<'_>, query_id: Option<&str>) -> fmt::Result {
    if let Some(query_id) = query_id {
        write!(formatter, " for query {query_id:?}")
    } else {
        formatter.write_str(" before Athena returned a query ID")
    }
}

fn write_query_context(
    formatter: &mut fmt::Formatter<'_>,
    query_id: Option<&str>,
    cancellation: CancellationOutcome,
) -> fmt::Result {
    write_query_id(formatter, query_id)?;
    write_cancellation_status(formatter, cancellation)
}

fn write_cancellation_status(
    formatter: &mut fmt::Formatter<'_>,
    cancellation: CancellationOutcome,
) -> fmt::Result {
    match cancellation {
        CancellationOutcome::NotNeeded => Ok(()),
        CancellationOutcome::Requested => formatter.write_str("; cancellation was requested"),
        CancellationOutcome::Failed => {
            formatter.write_str("; the cancellation request also failed")
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationOutcome {
    NotNeeded,
    Requested,
    Failed,
}
