pub mod athena;
pub mod auth;
pub mod cli;
mod config;
pub mod error;
mod output;
mod paths;
pub mod policy;
mod sql_input;

use crate::cli::{Cli, Command, ConfigCommand, SessionCommand};
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
        Command::Query(arguments) => {
            let config = Config::load(&config_path)?;
            let connection = config.connection(&arguments.data.connection)?;
            let prepared = sql_input::prepare(arguments)?;
            let output_plan = output::OutputPlan::resolve(&prepared.data).map_err(|error| {
                AppError::OutputArguments {
                    details: error.to_string(),
                }
            })?;
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
        Command::Catalogs(arguments)
        | Command::Databases(arguments)
        | Command::Tables {
            data: arguments, ..
        } => {
            verify_data_session(&config_path, &arguments).await?;
            Err(AppError::not_implemented("metadata discovery"))
        }
        Command::Describe(arguments) => {
            verify_data_session(&config_path, &arguments.data).await?;
            Err(AppError::not_implemented("metadata discovery"))
        }
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

async fn verify_data_session(
    config_path: &std::path::Path,
    arguments: &crate::cli::DataArgs,
) -> Result<(), AppError> {
    let config = Config::load(config_path)?;
    let connection = config.connection(&arguments.connection)?;
    auth::verify_session(
        connection,
        &arguments.session,
        paths::aws_credentials_file()?,
    )
    .await?;
    Ok(())
}
