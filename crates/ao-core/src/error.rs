use thiserror::Error;

pub type Result<T> = std::result::Result<T, AoError>;

#[derive(Debug, Error)]
pub enum AoError {
    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("runtime error: {0}")]
    Runtime(String),

    #[error("workspace error: {0}")]
    Workspace(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("scm error: {0}")]
    Scm(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("yaml: {0}")]
    Yaml(String),

    #[error("{0}")]
    Other(String),
}
