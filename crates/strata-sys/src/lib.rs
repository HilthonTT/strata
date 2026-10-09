//! System metrics for strata: disks and free space, disk I/O (IOPS,
//! throughput, latency), memory pressure, directory disk usage and Docker.
//!
//! Every collector is synchronous; the UI runs them on worker threads.

pub mod disks;
pub mod docker;
pub mod du;
pub mod history;
pub mod io;
pub mod memory;

pub use disks::{list_disks, DiskInfo};
pub use history::History;
pub use io::{IoSampler, IoStats};
pub use memory::{MemoryMonitor, MemorySnapshot, Pressure};
