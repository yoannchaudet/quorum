use serde::Serialize;
use thiserror::Error;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
pub struct AppError {
    pub code: String,
    pub message: String,
    #[ts(optional)]
    pub recovery: Option<String>,
}

impl AppError {
    pub fn path(message: impl Into<String>) -> Self {
        Self::new(
            "path",
            message,
            Some("Choose an accessible directory and try again."),
        )
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(
            "validation",
            message,
            Some("Check the provided values and try again."),
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message, None)
    }

    pub fn database(message: impl Into<String>) -> Self {
        Self::new(
            "database",
            message,
            Some("Your Quorum data was left unchanged. Check disk access and try again."),
        )
    }

    pub fn migration(message: impl Into<String>) -> Self {
        Self::new(
            "migration",
            message,
            Some("Quorum could not safely update its data. Please keep this database and contact support."),
        )
    }

    #[expect(
        dead_code,
        reason = "IPC conflict errors are reserved for optimistic state updates."
    )]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new("conflict", message, Some("Refresh and try again."))
    }

    fn new(code: impl Into<String>, message: impl Into<String>, recovery: Option<&str>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            recovery: recovery.map(str::to_owned),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("{0}")]
    App(#[from] AppError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<StoreError> for AppError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::App(error) => error,
            StoreError::Sqlite(error) => {
                Self::database(format!("Quorum could not access its database: {error}"))
            }
            StoreError::Io(error) => Self::path(format!(
                "Quorum could not access its data directory: {error}"
            )),
        }
    }
}
