use std::path::PathBuf;

use thiserror::Error;

use crate::athena::AthenaError;
use crate::auth::AuthError;
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

    #[error("{details}")]
    OutputArguments { details: String },

    #[error("{feature} is not implemented yet")]
    NotImplemented { feature: &'static str },
}

impl AppError {
    /// Constructs an error for a command whose local contract exists but whose
    /// external integration has not landed.
    #[must_use]
    pub const fn not_implemented(feature: &'static str) -> Self {
        Self::NotImplemented { feature }
    }

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
            | Self::NotImplemented { .. } => 2,
            Self::Authentication(_) => 3,
            Self::Athena(error) => error.exit_code(),
        }
    }
}
