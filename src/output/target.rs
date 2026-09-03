use std::fmt;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::cli::{DataArgs, OutputFormat};

#[derive(Debug)]
pub(crate) struct OutputPlan {
    pub(crate) format: OutputFormat,
    pub(crate) table_width: Option<usize>,
    destination: Destination,
}

#[derive(Debug)]
enum Destination {
    Stdout,
    File { path: PathBuf, force: bool },
}

impl OutputPlan {
    pub(crate) fn resolve(arguments: &DataArgs) -> Result<Self, OutputArgumentError> {
        let stdout_is_terminal = io::stdout().is_terminal();
        let table_width = stdout_is_terminal
            .then(terminal_size::terminal_size)
            .flatten()
            .map(|(terminal_size::Width(width), _)| usize::from(width));
        Self::resolve_with_terminal(
            arguments.format,
            arguments.output.as_deref(),
            arguments.force,
            stdout_is_terminal,
            table_width,
        )
    }

    fn resolve_with_terminal(
        requested: Option<OutputFormat>,
        output: Option<&Path>,
        force: bool,
        stdout_is_terminal: bool,
        table_width: Option<usize>,
    ) -> Result<Self, OutputArgumentError> {
        let extension_format = output.and_then(format_from_extension);
        let format = match (requested, output, extension_format) {
            (Some(requested), Some(path), Some(extension)) if requested != extension => {
                return Err(OutputArgumentError::FormatConflict {
                    requested,
                    path: path.to_path_buf(),
                    extension,
                });
            }
            (Some(requested), _, _) => requested,
            (None, Some(_), Some(extension)) => extension,
            (None, Some(path), None) => {
                return Err(OutputArgumentError::UnknownExtension {
                    path: path.to_path_buf(),
                });
            }
            (None, None, None) if stdout_is_terminal => OutputFormat::Table,
            (None, None, None) => OutputFormat::Jsonl,
            (None, None, Some(_)) => unreachable!("an extension requires an output path"),
        };

        let destination = if let Some(path) = output {
            if path.exists() && !force {
                return Err(OutputArgumentError::Exists {
                    path: path.to_path_buf(),
                });
            }
            Destination::File {
                path: path.to_path_buf(),
                force,
            }
        } else {
            Destination::Stdout
        };

        Ok(Self {
            format,
            table_width: (format == OutputFormat::Table)
                .then_some(table_width)
                .flatten(),
            destination,
        })
    }

    pub(crate) fn prepare(self) -> io::Result<PreparedOutput> {
        match self.destination {
            Destination::Stdout => Ok(PreparedOutput {
                format: self.format,
                table_width: self.table_width,
                sink: Sink::Stdout(io::stdout()),
            }),
            Destination::File { path, force } => {
                let parent = path
                    .parent()
                    .filter(|value| !value.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let temporary = tempfile::Builder::new()
                    .prefix(".honk-")
                    .suffix(".tmp")
                    .tempfile_in(parent)?;
                Ok(PreparedOutput {
                    format: self.format,
                    table_width: self.table_width,
                    sink: Sink::File {
                        temporary,
                        destination: path,
                        force,
                    },
                })
            }
        }
    }
}

pub(crate) struct PreparedOutput {
    pub(crate) format: OutputFormat,
    pub(crate) table_width: Option<usize>,
    sink: Sink,
}

enum Sink {
    Stdout(io::Stdout),
    File {
        temporary: tempfile::NamedTempFile,
        destination: PathBuf,
        force: bool,
    },
}

impl PreparedOutput {
    pub(crate) fn writer(&mut self) -> &mut dyn io::Write {
        match &mut self.sink {
            Sink::Stdout(stdout) => stdout,
            Sink::File { temporary, .. } => temporary.as_file_mut(),
        }
    }

    pub(crate) fn commit(self) -> io::Result<Option<PathBuf>> {
        match self.sink {
            Sink::Stdout(mut stdout) => {
                stdout.flush()?;
                Ok(None)
            }
            Sink::File {
                temporary,
                destination,
                force,
            } => {
                temporary.as_file().sync_all()?;
                let result = if force {
                    temporary.persist(&destination)
                } else {
                    temporary.persist_noclobber(&destination)
                };
                result.map_err(|error| error.error)?;
                Ok(Some(destination))
            }
        }
    }
}

fn format_from_extension(path: &Path) -> Option<OutputFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "csv" => Some(OutputFormat::Csv),
        "tsv" => Some(OutputFormat::Tsv),
        "json" => Some(OutputFormat::Json),
        "jsonl" | "ndjson" => Some(OutputFormat::Jsonl),
        "md" | "markdown" => Some(OutputFormat::Markdown),
        _ => None,
    }
}

#[derive(Debug, Error)]
pub(crate) enum OutputArgumentError {
    #[error("output file {path} already exists; pass --force to replace it")]
    Exists { path: PathBuf },

    #[error("cannot infer an output format from {path}; pass --format explicitly")]
    UnknownExtension { path: PathBuf },

    #[error(
        "requested format {requested} conflicts with the {extension} extension of output file {path}"
    )]
    FormatConflict {
        requested: OutputFormat,
        path: PathBuf,
        extension: OutputFormat,
    },
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Table => "table",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
            Self::Markdown => "markdown",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_and_pipe_have_different_defaults() {
        let terminal =
            OutputPlan::resolve_with_terminal(None, None, false, true, Some(94)).expect("terminal");
        assert_eq!(terminal.format, OutputFormat::Table);
        assert_eq!(terminal.table_width, Some(94));

        let pipe = OutputPlan::resolve_with_terminal(None, None, false, false, None).expect("pipe");
        assert_eq!(pipe.format, OutputFormat::Jsonl);
    }

    #[test]
    fn extensions_select_formats_and_conflicts_fail() {
        let csv = OutputPlan::resolve_with_terminal(
            None,
            Some(Path::new("result.csv")),
            false,
            false,
            None,
        )
        .expect("csv");
        assert_eq!(csv.format, OutputFormat::Csv);

        let error = OutputPlan::resolve_with_terminal(
            Some(OutputFormat::Jsonl),
            Some(Path::new("result.csv")),
            false,
            false,
            None,
        )
        .expect_err("conflict");
        assert!(matches!(error, OutputArgumentError::FormatConflict { .. }));
    }

    #[test]
    fn atomic_file_requires_force_and_commits_only_on_success() {
        let directory = tempfile::tempdir().expect("directory");
        let destination = directory.path().join("result.csv");
        std::fs::write(&destination, "old").expect("existing file");
        assert!(matches!(
            OutputPlan::resolve_with_terminal(None, Some(&destination), false, false, None),
            Err(OutputArgumentError::Exists { .. })
        ));

        let plan = OutputPlan::resolve_with_terminal(None, Some(&destination), true, false, None)
            .expect("forced plan");
        let mut output = plan.prepare().expect("temporary file");
        output.writer().write_all(b"new").expect("write");
        assert_eq!(
            std::fs::read_to_string(&destination).expect("old remains"),
            "old"
        );
        output.commit().expect("commit");
        assert_eq!(
            std::fs::read_to_string(destination).expect("new file"),
            "new"
        );
    }

    #[test]
    fn dropping_prepared_file_keeps_destination_absent() {
        let directory = tempfile::tempdir().expect("directory");
        let destination = directory.path().join("result.jsonl");
        let plan = OutputPlan::resolve_with_terminal(None, Some(&destination), false, false, None)
            .expect("plan");
        let mut output = plan.prepare().expect("temporary file");
        output.writer().write_all(b"partial").expect("write");
        drop(output);
        assert!(!destination.exists());
    }
}
