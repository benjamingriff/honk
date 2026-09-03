pub mod athena;
pub mod auth;
pub mod cli;
mod config;
pub mod error;
mod metadata;
mod output;
mod paths;
pub mod policy;
mod sql_input;

use crate::cli::{Cli, Command, ConfigCommand, DataArgs, DescribeArgs, QueryArgs, SessionCommand};
use crate::config::Config;
use crate::error::AppError;

/// Runs one parsed Honk command.
///
/// # Errors
///
/// Returns a typed error when invocation data, configuration, SQL input, the
/// read-only policy, or AWS session authentication is invalid. Later phases add
/// Athena failures to the same error boundary.
pub async fn run(cli: Cli) -> Result<(), AppError> {
    let config_path = paths::config_file()?;

    match cli.command {
        Command::Connections => {
            let config = Config::load(&config_path)?;
            print!("{}", config.render_connections());
            Ok(())
        }
        Command::Config {
            command: ConfigCommand::Check,
        } => {
            let config = Config::load(&config_path)?;
            println!(
                "Configuration is valid: {} connection{} in {}",
                config.len(),
                if config.len() == 1 { "" } else { "s" },
                config_path.display()
            );
            Ok(())
        }
        Command::Query(arguments) => run_query_command(&config_path, arguments).await,
        Command::Catalogs(arguments) => run_catalogs_command(&config_path, &arguments).await,
        Command::Databases(arguments) => run_databases_command(&config_path, &arguments).await,
        Command::Tables { data, database } => {
            run_tables_command(&config_path, &data, database.as_deref()).await
        }
        Command::Describe(arguments) => run_describe_command(&config_path, &arguments).await,
        Command::Session {
            command: SessionCommand::Check(arguments),
        } => {
            let config = Config::load(&config_path)?;
            let connection = config.connection(&arguments.connection)?;
            let session = auth::verify_session(
                connection,
                &arguments.session,
                paths::aws_credentials_file()?,
            )
            .await?;
            println!(
                "Session {:?} is valid for connection {:?}: account {}, role {}",
                arguments.session,
                arguments.connection,
                session.account(),
                session.role_name()
            );
            Ok(())
        }
    }
}

async fn run_query_command(
    config_path: &std::path::Path,
    arguments: QueryArgs,
) -> Result<(), AppError> {
    let config = Config::load(config_path)?;
    let connection = config.connection(&arguments.data.connection)?;
    let prepared = sql_input::prepare(arguments)?;
    let output_plan = resolve_output(&prepared.data)?;
    let session = auth::verify_session(
        connection,
        &prepared.data.session,
        paths::aws_credentials_file()?,
    )
    .await?;
    athena::run_query(
        connection,
        &prepared.data.session,
        &session,
        &prepared.query,
        output_plan,
        prepared.data.quiet,
    )
    .await?;
    Ok(())
}

async fn run_catalogs_command(
    config_path: &std::path::Path,
    arguments: &DataArgs,
) -> Result<(), AppError> {
    let (config, session, output_plan) = prepare_data_command(config_path, arguments).await?;
    let connection = config.connection(&arguments.connection)?;
    metadata::run_catalogs(
        connection,
        &arguments.session,
        &session,
        output_plan,
        arguments.quiet,
    )
    .await?;
    Ok(())
}

async fn run_databases_command(
    config_path: &std::path::Path,
    arguments: &DataArgs,
) -> Result<(), AppError> {
    let (config, session, output_plan) = prepare_data_command(config_path, arguments).await?;
    let connection = config.connection(&arguments.connection)?;
    metadata::run_databases(
        connection,
        &arguments.session,
        &session,
        output_plan,
        arguments.quiet,
    )
    .await?;
    Ok(())
}

async fn run_tables_command(
    config_path: &std::path::Path,
    arguments: &DataArgs,
    database: Option<&str>,
) -> Result<(), AppError> {
    let (config, session, output_plan) = prepare_data_command(config_path, arguments).await?;
    let connection = config.connection(&arguments.connection)?;
    let database = database.unwrap_or(&connection.database);
    metadata::run_tables(
        connection,
        &arguments.session,
        &session,
        database,
        output_plan,
        arguments.quiet,
    )
    .await?;
    Ok(())
}

async fn run_describe_command(
    config_path: &std::path::Path,
    arguments: &DescribeArgs,
) -> Result<(), AppError> {
    let config = Config::load(config_path)?;
    let connection = config.connection(&arguments.data.connection)?;
    let object =
        metadata::parse_object(&arguments.object, &connection.database).map_err(|details| {
            AppError::MetadataArguments {
                details: details.to_owned(),
            }
        })?;
    let output_plan = resolve_output(&arguments.data)?;
    let session = auth::verify_session(
        connection,
        &arguments.data.session,
        paths::aws_credentials_file()?,
    )
    .await?;
    metadata::run_describe(
        connection,
        &arguments.data.session,
        &session,
        &object,
        output_plan,
        arguments.data.quiet,
    )
    .await?;
    Ok(())
}

async fn prepare_data_command(
    config_path: &std::path::Path,
    arguments: &crate::cli::DataArgs,
) -> Result<(Config, auth::VerifiedSession, output::OutputPlan), AppError> {
    let config = Config::load(config_path)?;
    let connection = config.connection(&arguments.connection)?;
    let output_plan = resolve_output(arguments)?;
    let session = auth::verify_session(
        connection,
        &arguments.session,
        paths::aws_credentials_file()?,
    )
    .await?;
    Ok((config, session, output_plan))
}

fn resolve_output(arguments: &DataArgs) -> Result<output::OutputPlan, AppError> {
    output::OutputPlan::resolve(arguments).map_err(|error| AppError::OutputArguments {
        details: error.to_string(),
    })
}
