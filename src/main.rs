use std::process::ExitCode;

use clap::Parser;
use honk::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    match honk::run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
