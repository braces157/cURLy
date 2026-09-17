use thiserror::Error;

#[derive(Debug, Error)]
pub enum CurlyError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Config(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("operation cancelled")]
    Cancelled,
    #[error("history error: {0}")]
    History(String),
}

impl CurlyError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Invalid(_) | Self::Config(_) => 2,
            Self::Cancelled => 130,
            Self::Io(_) | Self::Transport(_) | Self::History(_) => 1,
        }
    }
}
