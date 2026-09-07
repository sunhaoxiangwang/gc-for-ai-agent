//! Error types shared across the core crate.

use std::path::PathBuf;

use thiserror::Error;

/// Anything that makes a configuration file unusable.
///
/// These map to exit code 2. They are separated from runtime errors because a
/// bad config is a user mistake with a specific fix, not an internal failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no configuration file found (looked at: {searched})\nrun `reap init --detect` to create one")]
    NotFound { searched: String },

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("{path}: {message}")]
    Invalid { path: PathBuf, message: String },

    #[error("could not determine the home directory: $HOME is unset, so `~` in {path} cannot be expanded")]
    NoHome { path: String },
}

/// Failures while planning. Planning is read-only, so these are always either
/// an unreadable path or an unusable rule.
#[derive(Debug, Error)]
pub enum PlanError {
    #[error("root {path} could not be resolved: {source}")]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
