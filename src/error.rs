//! Unified error type

use std::fmt;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    /// Windows API call failed, with context and the raw HRESULT
    Windows(&'static str, windows::core::Error),
    /// Any other error
    Other(String),
}

impl AppError {
    pub fn windows(ctx: &'static str, e: windows::core::Error) -> Self {
        AppError::Windows(ctx, e)
    }

    pub fn other(msg: impl Into<String>) -> Self {
        AppError::Other(msg.into())
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Windows(ctx, e) => write!(f, "Windows API failed [{}]: {}", ctx, e),
            AppError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Other(format!("IO: {}", e))
    }
}

impl From<windows::core::Error> for AppError {
    fn from(e: windows::core::Error) -> Self {
        AppError::Other(format!("Windows: {}", e))
    }
}
