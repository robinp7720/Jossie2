use thiserror::Error;

#[derive(Debug, Error)]
pub enum JossieError {
    #[error("LLM error: {0}")]
    Llm(String),
    #[error("Database error: {0}")]
    Database(String),
    #[error("Integration error: {0}")]
    Integration(String),
    #[error("Config error: {0}")]
    Config(String),
    #[error("Auth error: {0}")]
    Auth(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
