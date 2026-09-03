use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use honk::cli::Cli;

fn main() -> ExitCode {
    honk::install_panic_hook();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    if let Ok(runtime) = runtime {
        runtime.block_on(run())
    } else {
        let stderr = std::io::stderr();
        let _ = writeln!(
            stderr.lock(),
            "error: Honk could not start its async runtime"
        );
        ExitCode::FAILURE
    }
}

async fn run() -> ExitCode {
    match honk::run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let stderr = std::io::stderr();
            let _ = writeln!(stderr.lock(), "error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
