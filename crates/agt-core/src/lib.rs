//! agt 的核心：工作階段（PTY + VT 模擬 + 狀態機）、Agent Adapter 介面與平台差異。
//!
//! 平台差異（PTY、行程管理、通知、預設 shell）全部封裝在 [`platform`]，
//! `alacritty_terminal` 全部封裝在 [`term`]，其他模組不直接使用。

pub mod adapter;
pub mod osc;
pub mod platform;
pub mod session;
pub mod term;

pub use adapter::*;
pub use session::{Session, SessionEvent, SessionOptions, StatusSource};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn msg(s: impl Into<String>) -> Self {
        Error::Other(s.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
