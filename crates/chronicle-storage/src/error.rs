use std::io;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("corrupt record: {0}")]
    Corrupt(String),

    #[error("checksum mismatch: expected {expected:08x}, got {actual:08x}")]
    ChecksumMismatch { expected: u32, actual: u32 },

    #[error("serialization error: {0}")]
    Serde(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
