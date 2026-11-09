//! The crate's error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// The service answered with an error status; `body` is its message, cut to a readable size.
    #[error("{service} returned {status}: {body}")]
    Api { service: String, status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    #[error("no API key for {0}")]
    NoKey(String),
    #[error("{0}")]
    Invalid(String),
    #[error("tool: {0}")]
    Tool(String),
    #[error("stopped")]
    Cancelled,
    #[error("file: {0}")]
    Io(#[from] std::io::Error),
    #[error("data: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::Network(e.to_string())
    }
}

/// `s` cut to at most `max` characters (on a character boundary), with an ellipsis when cut.
pub fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", s.get(..i).unwrap_or(s)),
        None => s.to_string(),
    }
}
