use harness_types::{ErrorCode, HarnessError};
use thiserror::Error;

/// A stable error emitted by the P1 `SQLite` boundary.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{code}: {message}")]
pub struct StoreError {
    code: ErrorCode,
    message: String,
}

impl StoreError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub fn into_harness_error(self) -> HarnessError {
        HarnessError::new(self.code, self.message)
    }
}
