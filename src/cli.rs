use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "honk",
    version,
    about = "Run read-only queries against an Athena lakehouse",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run one read-only SQL statement.
    Query(QueryArgs),

    /// List Athena data catalogs available to the connection.
    Catalogs(DataArgs),

    /// List databases in the configured catalog.
    Databases(DataArgs),

    /// List tables in a database.
    Tables {
        #[command(flatten)]
        data: DataArgs,

        /// Override the connection's configured database.
        #[arg(long, value_name = "NAME", value_parser = non_blank)]
        database: Option<String>,
    },

    /// Describe a table or view.
    Describe(DescribeArgs),

    /// List configured connections without credentials.
    Connections,

    /// Check Honk configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Check an existing AWS session profile.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    #[command(flatten)]
    pub data: DataArgs,

    /// Read SQL from this file instead of the positional argument or stdin.
    #[arg(long, value_name = "PATH", conflicts_with = "sql")]
    pub file: Option<PathBuf>,

    /// SQL supplied directly on the command line. Omit it to read stdin.
    #[arg(value_name = "SQL")]
    pub sql: Option<String>,
}

#[derive(Debug, Args)]
pub struct DescribeArgs {
    #[command(flatten)]
    pub data: DataArgs,

    /// Table to describe, optionally qualified by database.
    #[arg(value_name = "[DATABASE.]TABLE", value_parser = non_blank)]
    pub object: String,
}

#[derive(Debug, Args)]
pub struct DataArgs {
    /// Honk connection name.
    #[arg(long, value_name = "NAME", value_parser = non_blank)]
    pub connection: String,

    /// Existing AWS profile containing temporary session credentials.
    #[arg(long, value_name = "PROFILE", value_parser = non_blank)]
    pub session: String,

    /// Result format. Honk chooses table for a terminal and JSON Lines otherwise.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<OutputFormat>,

    /// Write results to this path.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Replace an existing output file.
    #[arg(long, requires = "output")]
    pub force: bool,

    /// Suppress successful operational messages on stderr.
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Csv,
    Tsv,
    Json,
    Jsonl,
    Markdown,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Parse and validate the complete configuration file.
    Check,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Verify a named AWS profile against a connection.
    Check(SessionArgs),
}

#[derive(Debug, Args)]
pub struct SessionArgs {
    /// Honk connection name.
    #[arg(long, value_name = "NAME", value_parser = non_blank)]
    pub connection: String,

    /// Existing AWS profile containing temporary session credentials.
    #[arg(long, value_name = "PROFILE", value_parser = non_blank)]
    pub session: String,
}

fn non_blank(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("value cannot be empty or whitespace".to_owned())
    } else {
        Ok(value.to_owned())
    }
}
