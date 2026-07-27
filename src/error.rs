use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("not connected to reMarkable Cloud")]
    NotConnected,

    #[error("{0} not found")]
    NotFound(String),

    #[error("{0} is ambiguous; use its full path")]
    Ambiguous(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("reMarkable Cloud rejected the request: {0}")]
    Cloud(String),

    #[error("unsupported document: {0}")]
    Unsupported(String),

    #[error("render failed: {0}")]
    Render(String),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
