//! `chronicle-storage`: the LSM-tree storage engine underneath Chronicle.
//!
//! Built up incrementally, module by module -- see git history. This file
//! only re-exports what exists so far.

pub mod error;
pub use error::{Result, StorageError};
