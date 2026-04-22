//! Shared error type. Library code uses `Result<T, Error>`; the
//! `impl From<Error> for PyErr` maps each variant to an appropriate
//! Python exception so clients see actionable MCP errors instead of
//! opaque `INTERNAL_ERROR`.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("query parse error: {0}")]
    QueryParse(String),

    #[error("regex too complex for DFA-only engine: {0}")]
    RegexComplexity(String),

    #[error("query exceeded {limit_ms} ms wall-clock limit")]
    QueryTimeout { limit_ms: u64 },

    #[error("query cancelled by caller")]
    QueryCancelled,

    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("gix error: {0}")]
    #[allow(dead_code)]
    Gix(String),

    #[error("mail parse error: {0}")]
    #[allow(dead_code)]
    MailParse(String),

    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("state inconsistency: {0}")]
    State(String),

    #[error("sync error: {0}")]
    Sync(String),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("msgpack encode error: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),

    #[error("msgpack decode error: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),
}

impl From<Error> for PyErr {
    fn from(e: Error) -> Self {
        match e {
            Error::QueryParse(_) | Error::RegexComplexity(_) | Error::InvalidCursor(_) => {
                PyValueError::new_err(e.to_string())
            }
            _ => PyRuntimeError::new_err(e.to_string()),
        }
    }
}

#[allow(dead_code)] // consumed by downstream modules in follow-up PRs
pub type Result<T> = std::result::Result<T, Error>;
