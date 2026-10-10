//! Core domain of strata: the virtual filesystem layer, file operations,
//! background jobs, fuzzy search and NAS connection handling.
//!
//! Nothing in this crate knows about the terminal UI.

pub mod bulk;
pub mod entry;
pub mod git;
pub mod inspect;
pub mod jobs;
pub mod nas;
pub mod ops;
pub mod perm;
pub mod search;
pub mod secrets;
pub mod sort;
pub mod util;
pub mod vfs;

pub use entry::{Entry, EntryKind};
pub use vfs::{Vfs, VfsRef};
