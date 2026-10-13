//! Bandwidth limits: rates such as `10M`, and pacing a copy loop to one.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};

/// Parses a rate in bytes per second, written like rclone's `--bwlimit`:
/// a number with an optional binary unit (`B`, `K`, `M`, `G`, `T`; `KB`,
/// `KiB` and a trailing `/s` are accepted too). A bare number is KiB/s.
/// Empty, `0` and `off` mean no limit (`None`).
pub fn parse_rate(text: &str) -> Result<Option<u64>> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    if matches!(lower.as_str(), "" | "0" | "off" | "none" | "unlimited") {
        return Ok(None);
    }
    let lower = lower.strip_suffix("/s").unwrap_or(&lower);
    let split = lower.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(lower.len());
    let (number, unit) = lower.split_at(split);
    let unit = unit.trim();
    let unit = unit.strip_suffix("ib").or_else(|| unit.strip_suffix('b').filter(|u| !u.is_empty())).unwrap_or(unit);
    let shift = match unit {
        "b" => 0,
        "" | "k" => 10,
        "m" => 20,
        "g" => 30,
        "t" => 40,
        _ => bail!("unknown unit in '{text}': use B, K, M, G or T, e.g. 10M"),
    };
    let value: f64 = number.parse().map_err(|_| anyhow::anyhow!("'{text}' is not a rate, e.g. 10M or 512K"))?;
    let bytes = (value * (1u64 << shift) as f64).round();
    if bytes < 1.0 {
        bail!("'{text}' is too small a rate");
    }
    Ok(Some(bytes as u64))
}

/// A rate as rclone's `--bwlimit` takes it, in whole KiB/s.
pub fn rclone_rate(bytes_per_sec: u64) -> String {
    format!("{}K", bytes_per_sec.div_ceil(1024))
}

/// Keeps a byte stream at or under a rate: after each chunk, says how long
/// to wait. Falling behind (a slow source, a pause) is not made up for
/// with a burst: past a second behind, the count starts over.
#[derive(Debug, Clone)]
pub struct Throttle {
    rate: u64,
    /// When counting started, and the bytes since.
    since: Option<Instant>,
    sent: u64,
}

impl Throttle {
    /// How far behind schedule the stream may fall before it starts over.
    const SLACK: Duration = Duration::from_secs(1);

    pub fn new(bytes_per_sec: u64) -> Self {
        Self { rate: bytes_per_sec.max(1), since: None, sent: 0 }
    }

    /// Records `n` bytes sent at `now`; returns how long to wait before
    /// sending more.
    pub fn delay(&mut self, n: u64, now: Instant) -> Duration {
        let since = *self.since.get_or_insert(now);
        self.sent += n;
        let due = since + Duration::from_secs_f64(self.sent as f64 / self.rate as f64);
        if due > now {
            return due - now;
        }
        if now - due > Self::SLACK {
            self.since = Some(now);
            self.sent = 0;
        }
        Duration::ZERO
    }

    /// Bytes to read at a time: about a tenth of a second's worth, so the
    /// pace is smooth even at low rates.
    pub fn chunk(&self, max: usize) -> usize {
        ((self.rate / 10) as usize).clamp(4096.min(max), max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rates_like_rclone() {
        assert_eq!(parse_rate("10M").unwrap(), Some(10 << 20));
        assert_eq!(parse_rate("1.5m").unwrap(), Some(3 << 19));
        assert_eq!(parse_rate("512K").unwrap(), Some(512 << 10));
        assert_eq!(parse_rate("512 KiB/s").unwrap(), Some(512 << 10));
        assert_eq!(parse_rate("2GB").unwrap(), Some(2 << 30));
        assert_eq!(parse_rate("100B").unwrap(), Some(100));
        assert_eq!(parse_rate("64").unwrap(), Some(64 << 10), "a bare number is KiB/s");
        for off in ["", " ", "off", "0", "OFF"] {
            assert_eq!(parse_rate(off).unwrap(), None);
        }
        for bad in ["fast", "10X", "M", "-1M", "0.0001B"] {
            assert!(parse_rate(bad).is_err(), "{bad}");
        }
        assert_eq!(rclone_rate(10 << 20), "10240K");
        assert_eq!(rclone_rate(100), "1K");
    }

    #[test]
    fn throttle_keeps_to_the_rate() {
        let t0 = Instant::now();
        let mut t = Throttle::new(1000);
        // 500 bytes at once: wait until half a second has passed.
        assert_eq!(t.delay(500, t0), Duration::from_millis(500));
        // Having waited, another 500 are due at the one second mark.
        assert_eq!(t.delay(500, t0 + Duration::from_millis(500)), Duration::from_millis(500));
        // Slower than the rate: no wait.
        assert_eq!(t.delay(100, t0 + Duration::from_millis(1500)), Duration::ZERO);
    }

    #[test]
    fn throttle_does_not_burst_after_a_stall() {
        let t0 = Instant::now();
        let mut t = Throttle::new(1000);
        assert_eq!(t.delay(1000, t0), Duration::from_secs(1));
        // Paused for a minute: the next second's worth is paced again
        // instead of going out at once to catch up.
        let later = t0 + Duration::from_secs(61);
        assert_eq!(t.delay(0, later), Duration::ZERO);
        assert_eq!(t.delay(1000, later), Duration::from_secs(1));
    }

    #[test]
    fn chunks_shrink_at_low_rates() {
        assert_eq!(Throttle::new(10 << 20).chunk(256 * 1024), 256 * 1024);
        assert_eq!(Throttle::new(1 << 20).chunk(256 * 1024), 104_857);
        assert_eq!(Throttle::new(1000).chunk(256 * 1024), 4096);
    }
}
