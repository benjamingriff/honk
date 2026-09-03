use std::path::PathBuf;

use thiserror::Error;

use crate::athena::AthenaError;
use crate::auth::AuthError;
use crate::metadata::MetadataError;
use crate::policy::athena::PolicyError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("cannot locate the Honk configuration file because HOME is not set")]
    MissingHome,

    #[error("cannot read configuration {path}: {source}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid configuration {path}:\n{details}")]
    InvalidConfig { path: PathBuf, details: String },

    #[error("unknown connection {name:?}; configured connections: {available}")]
    UnknownConnection { name: String, available: String },

    #[error("no SQL was supplied; pass SQL as an argument, use --file, or pipe it to stdin")]
    MissingSqlInput,

    #[error("cannot read SQL file {path}: {source}")]
    ReadSqlFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot read SQL from {source_name}: {source}")]
    ReadSqlInput {
        source_name: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{source_name} is not valid UTF-8")]
    SqlInputNotUtf8 { source_name: String },

    #[error(transparent)]
    Policy(#[from] PolicyError),

    #[error(transparent)]
    Authentication(#[from] AuthError),

    #[error(transparent)]
    Athena(#[from] AthenaError),

    #[error(transparent)]
    Metadata(#[from] MetadataError),

    #[error("{details}")]
    OutputArguments { details: String },

    #[error("{details}")]
    MetadataArguments { details: String },

    #[error(
        "{command} requires a catalog; pass --catalog or set default_catalog on the connection"
    )]
    MissingCatalog { command: &'static str },

    #[error(
        "{command} requires a database; pass --database or set default_database on the connection"
    )]
    MissingDatabase { command: &'static str },

    #[error("cannot write command output: {source}")]
    CommandOutput {
        #[source]
        source: std::io::Error,
    },
}

impl AppError {
    /// Returns the stable process exit code for this error category.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::MissingHome
            | Self::ReadConfig { .. }
            | Self::InvalidConfig { .. }
            | Self::UnknownConnection { .. }
            | Self::MissingSqlInput
            | Self::ReadSqlFile { .. }
            | Self::ReadSqlInput { .. }
            | Self::SqlInputNotUtf8 { .. }
            | Self::Policy(_)
            | Self::OutputArguments { .. }
            | Self::MetadataArguments { .. }
            | Self::MissingCatalog { .. }
            | Self::MissingDatabase { .. } => 2,
            Self::Authentication(_) => 3,
            Self::Athena(error) => error.exit_code(),
            Self::Metadata(error) => error.exit_code(),
            Self::CommandOutput { .. } => 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::athena::{CancellationOutcome, Operation};
    use crate::metadata;

    #[test]
    fn every_public_exit_category_has_its_stable_code() {
        let errors = [
            (AppError::MissingHome, 2),
            (
                AuthError::ProfileUnavailable {
                    profile: "test-session".into(),
                }
                .into(),
                3,
            ),
            (
                AthenaError::OperationFailed {
                    operation: Operation::GetWorkGroup,
                    query_id: None,
                    cancellation: CancellationOutcome::NotNeeded,
                }
                .into(),
                4,
            ),
            (
                AthenaError::Output {
                    query_id: "test-query".into(),
                    source: io::Error::other("test output failure"),
                }
                .into(),
                5,
            ),
            (
                AthenaError::Interrupted {
                    query_id: Some("test-query".into()),
                    cancellation: CancellationOutcome::Requested,
                }
                .into(),
                130,
            ),
            (
                metadata::MetadataError::CredentialsExpired {
                    operation: metadata::Operation::Catalogs,
                }
                .into(),
                3,
            ),
            (
                metadata::MetadataError::ProviderFailed {
                    operation: metadata::Operation::Tables,
                    catalog: Some("test-catalog".into()),
                }
                .into(),
                4,
            ),
            (
                metadata::MetadataError::Output {
                    source: io::Error::other("test output failure"),
                }
                .into(),
                5,
            ),
            (
                AppError::CommandOutput {
                    source: io::Error::new(io::ErrorKind::BrokenPipe, "test broken pipe"),
                },
                5,
            ),
        ];
        for (error, expected) in errors {
            assert_eq!(error.exit_code(), expected, "{error}");
        }
    }
}
