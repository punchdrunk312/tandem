use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Proto(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("rejected by peer: {0}")]
    Rejected(String),
    #[error("frame too large")]
    TooLarge,
    #[error("closed")]
    Closed,
}

pub type Result<T> = std::result::Result<T, Error>;
