//! Memory usage and pressure. Linux exposes PSI (`/proc/pressure/memory`):
//! the share of time tasks stalled waiting for memory, which is a far better
//! signal than "used" percentages.

use sysinfo::{MemoryRefreshKind, RefreshKind, System};

use crate::History;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Psi {
    /// % of time at least one task stalled on memory (10s / 60s / 300s averages).
    pub some_avg10: f64,
    pub some_avg60: f64,
    pub some_avg300: f64,
    /// % of time all non-idle tasks stalled simultaneously.
    pub full_avg10: f64,
    pub full_avg60: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Elevated,
    High,
    Critical,
}

impl Pressure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Elevated => "elevated",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemorySnapshot {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub psi: Option<Psi>,
    pub pressure: Pressure,
    /// Used memory in percent over time.
    pub usage_history: History,
    /// PSI `some avg10` ×100 over time.
    pub psi_history: History,
}

impl MemorySnapshot {
    pub fn used_ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used as f64 / self.total as f64
        }
    }

    pub fn swap_ratio(&self) -> f64 {
        if self.swap_total == 0 {
            0.0
        } else {
            self.swap_used as f64 / self.swap_total as f64
        }
    }
}

pub struct MemoryMonitor {
    system: System,
    snapshot: MemorySnapshot,
}

impl Default for MemoryMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMonitor {
    pub fn new() -> Self {
        let system = System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()));
        Self { system, snapshot: MemorySnapshot::default() }
    }

    pub fn sample(&mut self) -> MemorySnapshot {
        self.system.refresh_memory();
        let s = &mut self.snapshot;
        s.total = self.system.total_memory();
        s.available = self.system.available_memory();
        s.used = s.total.saturating_sub(s.available);
        s.swap_total = self.system.total_swap();
        s.swap_used = self.system.used_swap();
        s.psi = read_psi();
        s.pressure = classify(s.psi, s.available as f64 / s.total.max(1) as f64, s.swap_ratio());
        s.usage_history.push((s.used_ratio() * 100.0).round() as u64);
        s.psi_history.push(s.psi.map(|p| (p.some_avg10 * 100.0).round() as u64).unwrap_or(0));
        s.clone()
    }
}

/// Pressure from PSI when available, otherwise from free memory and swap.
pub fn classify(psi: Option<Psi>, available_ratio: f64, swap_ratio: f64) -> Pressure {
    if let Some(p) = psi {
        return match () {
            _ if p.full_avg10 >= 10.0 || p.some_avg10 >= 40.0 => Pressure::Critical,
            _ if p.full_avg10 >= 2.0 || p.some_avg10 >= 15.0 => Pressure::High,
            _ if p.some_avg10 >= 2.0 || p.some_avg60 >= 5.0 => Pressure::Elevated,
            _ => Pressure::Normal,
        };
    }
    match () {
        _ if available_ratio < 0.05 => Pressure::Critical,
        _ if available_ratio < 0.10 || swap_ratio > 0.5 => Pressure::High,
        _ if available_ratio < 0.20 => Pressure::Elevated,
        _ => Pressure::Normal,
    }
}

fn read_psi() -> Option<Psi> {
    let text = std::fs::read_to_string("/proc/pressure/memory").ok()?;
    parse_psi(&text)
}

pub fn parse_psi(text: &str) -> Option<Psi> {
    let mut psi = Psi::default();
    let mut found = false;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let kind = parts.next()?;
        for kv in parts {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            let v: f64 = v.parse().unwrap_or(0.0);
            found = true;
            match (kind, k) {
                ("some", "avg10") => psi.some_avg10 = v,
                ("some", "avg60") => psi.some_avg60 = v,
                ("some", "avg300") => psi.some_avg300 = v,
                ("full", "avg10") => psi.full_avg10 = v,
                ("full", "avg60") => psi.full_avg60 = v,
                _ => {}
            }
        }
    }
    found.then_some(psi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_psi() {
        let p =
            parse_psi("some avg10=1.50 avg60=0.20 avg300=0.00 total=1\nfull avg10=0.30 avg60=0.00 avg300=0.00 total=0")
                .unwrap();
        assert_eq!(p.some_avg10, 1.5);
        assert_eq!(p.full_avg10, 0.3);
    }

    #[test]
    fn classifies_pressure() {
        assert_eq!(classify(None, 0.5, 0.0), Pressure::Normal);
        assert_eq!(classify(None, 0.03, 0.0), Pressure::Critical);
        let psi = Psi { some_avg10: 20.0, ..Default::default() };
        assert_eq!(classify(Some(psi), 0.9, 0.0), Pressure::High);
    }
}
