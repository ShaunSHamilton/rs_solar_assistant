#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Transport(#[from] reqwest::Error),
    #[error("{0}")]
    Serialization(#[from] serde_json::Error),
}
