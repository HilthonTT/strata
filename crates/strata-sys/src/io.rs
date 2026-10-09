//! Per-device I/O rates. On Linux this reads `/proc/diskstats`, which also
//! gives operation counts and time spent, hence IOPS and average latency.
//! Elsewhere it falls back to byte counters from `sysinfo`.

use std::collections::HashMap;
use std::time::Instant;

use crate::History;

#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    reads: u64,
    read_sectors: u64,
    read_ms: u64,
    writes: u64,
    write_sectors: u64,
    write_ms: u64,
    io_ms: u64,
}

/// Rates for one device over the last sampling interval.
#[derive(Debug, Clone, Default)]
pub struct IoStats {
    pub device: String,
    pub read_iops: f64,
    pub write_iops: f64,
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
    /// Average time per read, in milliseconds.
    pub read_latency_ms: f64,
    pub write_latency_ms: f64,
    /// Percentage of time the device was busy.
    pub utilization: f64,
    pub iops_history: History,
    pub latency_history: History,
    /// Whether IOPS/latency are real measurements (Linux only).
    pub detailed: bool,
}

#[derive(Default)]
pub struct IoSampler {
    previous: HashMap<String, Counters>,
    stats: HashMap<String, IoStats>,
    last: Option<Instant>,
    #[cfg(not(target_os = "linux"))]
    disks: Option<sysinfo::Disks>,
}

impl IoSampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a new sample and returns rates since the previous one.
    pub fn sample(&mut self) -> Vec<IoStats> {
        let now = Instant::now();
        let elapsed = self.last.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        self.last = Some(now);
        let current = read_counters(self);

        for (device, cur) in &current {
            let stats = self.stats.entry(device.clone()).or_insert_with(|| IoStats {
                device: device.clone(),
                detailed: cfg!(target_os = "linux"),
                ..Default::default()
            });
            if let (Some(prev), true) = (self.previous.get(device), elapsed > 0.0) {
                let d = |a: u64, b: u64| a.saturating_sub(b) as f64;
                let reads = d(cur.reads, prev.reads);
                let writes = d(cur.writes, prev.writes);
                stats.read_iops = reads / elapsed;
                stats.write_iops = writes / elapsed;
                stats.read_bytes_per_sec = d(cur.read_sectors, prev.read_sectors) * 512.0 / elapsed;
                stats.write_bytes_per_sec = d(cur.write_sectors, prev.write_sectors) * 512.0 / elapsed;
                stats.read_latency_ms = if reads > 0.0 { d(cur.read_ms, prev.read_ms) / reads } else { 0.0 };
                stats.write_latency_ms = if writes > 0.0 { d(cur.write_ms, prev.write_ms) / writes } else { 0.0 };
                stats.utilization = (d(cur.io_ms, prev.io_ms) / (elapsed * 1000.0) * 100.0).min(100.0);
                stats.iops_history.push((stats.read_iops + stats.write_iops).round() as u64);
                let lat = stats.read_latency_ms.max(stats.write_latency_ms);
                // Stored in microseconds so sub-millisecond SSD latency still shows.
                stats.latency_history.push((lat * 1000.0).round() as u64);
            }
        }
        self.stats.retain(|k, _| current.contains_key(k));
        self.previous = current;

        let mut out: Vec<IoStats> = self.stats.values().cloned().collect();
        out.sort_by(|a, b| a.device.cmp(&b.device));
        out
    }
}

#[cfg(target_os = "linux")]
fn read_counters(_: &mut IoSampler) -> HashMap<String, Counters> {
    let Ok(text) = std::fs::read_to_string("/proc/diskstats") else {
        return HashMap::new();
    };
    parse_diskstats(&text)
}

#[cfg(target_os = "linux")]
fn parse_diskstats(text: &str) -> HashMap<String, Counters> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 14 {
                return None;
            }
            let name = f[2];
            if !is_physical(name) {
                return None;
            }
            let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
            Some((
                name.to_string(),
                Counters {
                    reads: n(3),
                    read_sectors: n(5),
                    read_ms: n(6),
                    writes: n(7),
                    write_sectors: n(9),
                    write_ms: n(10),
                    io_ms: n(12),
                },
            ))
        })
        .collect()
}

/// Whole block devices only: no partitions, loop or ram devices.
#[cfg(target_os = "linux")]
fn is_physical(name: &str) -> bool {
    if ["loop", "ram", "zram", "dm-", "sr"].iter().any(|p| name.starts_with(p)) {
        return false;
    }
    let sys = std::path::Path::new("/sys/block").join(name);
    if std::path::Path::new("/sys/block").exists() {
        return sys.exists();
    }
    !name.chars().last().is_some_and(|c| c.is_ascii_digit())
}

#[cfg(not(target_os = "linux"))]
fn read_counters(sampler: &mut IoSampler) -> HashMap<String, Counters> {
    let disks = sampler.disks.get_or_insert_with(sysinfo::Disks::new_with_refreshed_list);
    disks.refresh(true);
    disks
        .list()
        .iter()
        .map(|d| {
            let u = d.usage();
            (
                d.name().to_string_lossy().into_owned(),
                Counters {
                    read_sectors: u.total_read_bytes / 512,
                    write_sectors: u.total_written_bytes / 512,
                    ..Default::default()
                },
            )
        })
        .collect()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parses_diskstats_lines() {
        let text =
            "   8       0 fakedisk 100 0 2000 50 40 0 800 20 0 70 70 0 0 0 0\n   7       0 loop0 1 0 1 1 1 0 1 1 0 1 1";
        // `fakedisk` is not in /sys/block, so it is filtered on real systems.
        let parsed = parse_diskstats(text);
        assert!(!parsed.contains_key("loop0"));
    }
}
