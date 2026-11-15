//! Error type shared by the core library.

use std::path::PathBuf;

use crate::protocol::{PairRejection, RemoteError};

/// Result alias for the core library.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong in the core library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is damaged: {message}")]
    CorruptFile { path: PathBuf, message: String },

    #[error("network: {0}")]
    Network(String),

    #[error("the other side broke the protocol: {0}")]
    Protocol(String),

    #[error("{0}")]
    Remote(RemoteError),

    #[error("pairing failed: {0}")]
    PairingRejected(PairRejection),

    #[error("invalid pairing ticket: {0}")]
    InvalidTicket(String),

    #[error("no paired machine matches \"{0}\"")]
    UnknownMachine(String),

    #[error("\"{query}\" matches more than one machine: {matches}")]
    AmbiguousMachine { query: String, matches: String },

    #[error("timed out: {0}")]
    Timeout(&'static str),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn network(err: impl std::fmt::Display) -> Self {
        Error::Network(err.to_string())
    }

    pub(crate) fn protocol(err: impl std::fmt::Display) -> Self {
        Error::Protocol(err.to_string())
    }
}
