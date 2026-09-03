pub mod athena;
pub mod auth;
pub mod cli;
mod config;
mod diagnostics;
pub mod error;
mod metadata;
mod output;
mod paths;
pub mod policy;
mod sql_input;

use std::io::Write as _;

use crate::cli::{
    CatalogArgs, Cli, Command, ConfigCommand, DataArgs, DescribeArgs, NamespaceCommandArgs,
    QueryArgs, SessionCommand,
};
use crate::config::Config;
pub use crate::diagnostics::install_panic_hook;
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
            std::io::stdout()
                .lock()
                .write_all(config.render_connections().as_bytes())
                .map_err(|source| AppError::CommandOutput { source })?;
            Ok(())
        }
        Command::Config {
            command: ConfigCommand::Check,
        } => {
            let config = Config::load(&config_path)?;
            writeln!(
                std::io::stdout().lock(),
                "Configuration is valid: {} connection{} in {}",
                config.len(),
                if config.len() == 1 { "" } else { "s" },
                config_path.display()
            )
            .map_err(|source| AppError::CommandOutput { source })?;
            Ok(())
        }
        Command::Query(arguments) => run_query_command(&config_path, arguments).await,
        Command::Catalogs(arguments) => run_catalogs_command(&config_path, &arguments).await,
        Command::Databases(arguments) => run_databases_command(&config_path, &arguments).await,
        Command::Tables(arguments) => run_tables_command(&config_path, &arguments).await,
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
            writeln!(
                std::io::stdout().lock(),
                "Session {:?} is valid for connection {:?}: account {}, role {}",
                arguments.session,
                arguments.connection,
                session.account(),
                session.role_name()
            )
            .map_err(|source| AppError::CommandOutput { source })?;
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
    let catalog = prepared
        .namespace
        .catalog
        .as_deref()
        .or(connection.default_catalog.as_deref());
    let database = prepared
        .namespace
        .database
        .as_deref()
        .or(connection.default_database.as_deref());
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
        athena::Namespace { catalog, database },
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
    arguments: &CatalogArgs,
) -> Result<(), AppError> {
    let config = Config::load(config_path)?;
    let connection = config.connection(&arguments.data.connection)?;
    let catalog = required_catalog(arguments.catalog.as_deref(), connection, "databases")?;
    let output_plan = resolve_output(&arguments.data)?;
    let session = verify_data_session(connection, &arguments.data.session).await?;
    metadata::run_databases(
        connection,
        &arguments.data.session,
        &session,
        catalog,
        output_plan,
        arguments.data.quiet,
    )
    .await?;
    Ok(())
}

async fn run_tables_command(
    config_path: &std::path::Path,
    arguments: &NamespaceCommandArgs,
) -> Result<(), AppError> {
    let config = Config::load(config_path)?;
    let connection = config.connection(&arguments.data.connection)?;
    let catalog = required_catalog(arguments.namespace.catalog.as_deref(), connection, "tables")?;
    let database = required_database(
        arguments.namespace.database.as_deref(),
        connection,
        "tables",
    )?;
    let output_plan = resolve_output(&arguments.data)?;
    let session = verify_data_session(connection, &arguments.data.session).await?;
    metadata::run_tables(
        connection,
        &arguments.data.session,
        &session,
        catalog,
        database,
        output_plan,
        arguments.data.quiet,
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
    let catalog = required_catalog(
        arguments.namespace.catalog.as_deref(),
        connection,
        "describe",
    )?;
    let object = metadata::parse_object(
        &arguments.object,
        arguments.namespace.database.as_deref(),
        connection.default_database.as_deref(),
    )
    .map_err(|details| AppError::MetadataArguments {
        details: details.to_owned(),
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
        catalog,
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

async fn verify_data_session(
    connection: &crate::config::Connection,
    session: &str,
) -> Result<auth::VerifiedSession, AppError> {
    Ok(auth::verify_session(connection, session, paths::aws_credentials_file()?).await?)
}

fn required_catalog<'a>(
    requested: Option<&'a str>,
    connection: &'a crate::config::Connection,
    command: &'static str,
) -> Result<&'a str, AppError> {
    requested
        .or(connection.default_catalog.as_deref())
        .ok_or(AppError::MissingCatalog { command })
}

fn required_database<'a>(
    requested: Option<&'a str>,
    connection: &'a crate::config::Connection,
    command: &'static str,
) -> Result<&'a str, AppError> {
    requested
        .or(connection.default_database.as_deref())
        .ok_or(AppError::MissingDatabase { command })
}

fn resolve_output(arguments: &DataArgs) -> Result<output::OutputPlan, AppError> {
    output::OutputPlan::resolve(arguments).map_err(|error| AppError::OutputArguments {
        details: error.to_string(),
    })
}
