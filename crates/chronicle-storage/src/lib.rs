//! `chronicle-storage`: the LSM-tree storage engine underneath Chronicle.
//!
//! Built up incrementally, module by module -- see git history.

pub mod error;
pub mod wal;

pub use error::{Result, StorageError};
pub use wal::{WalRecord, WriteAheadLog};
