//! Core domain of strata: the virtual filesystem layer, file operations,
//! background jobs, fuzzy search and NAS connection handling.
//!
//! Nothing in this crate knows about the terminal UI.

pub mod bulk;
pub mod compare;
pub mod compress;
pub mod dupes;
pub mod elevate;
pub mod entry;
pub mod filter;
pub mod find;
pub mod frecency;
pub mod git;
pub mod inspect;
pub mod jobs;
pub mod nas;
pub mod ops;
pub mod pattern;
pub mod perm;
pub mod search;
pub mod secrets;
pub mod sort;
pub mod sync;
pub mod tags;
pub mod throttle;
pub mod trash;
pub mod util;
pub mod vfs;

pub use entry::{Entry, EntryKind};
pub use vfs::{Vfs, VfsRef};
