//! Background jobs (copy, move, delete...) with shared progress counters.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    Failed(String),
    Cancelled,
}

/// Progress shared between the worker thread and the UI.
#[derive(Debug, Default)]
pub struct Progress {
    pub total_bytes: AtomicU64,
    pub done_bytes: AtomicU64,
    pub total_items: AtomicUsize,
    pub done_items: AtomicUsize,
    cancelled: AtomicBool,
    paused: AtomicBool,
    current: Mutex<String>,
    /// Sampled by the UI as it draws; see [`Progress::throughput`].
    meter: Mutex<RateMeter>,
}

impl Progress {
    pub fn add_bytes(&self, n: u64) {
        self.done_bytes.fetch_add(n, Ordering::Relaxed);
    }

    pub fn item_done(&self) {
        self.done_items.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_current(&self, s: impl Into<String>) {
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
    }

    pub fn current(&self) -> String {
        self.current.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Pauses or resumes the job at its next [`Progress::check`].
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Bails out of a job loop when the user cancelled it, and waits here
    /// while the job is paused.
    pub fn check(&self) -> Result<()> {
        while self.is_paused() && !self.is_cancelled() {
            thread::sleep(Duration::from_millis(100));
        }
        if self.is_cancelled() {
            bail!(Cancelled);
        }
        Ok(())
    }

    /// Sleeps for `duration`, waking early when the job is cancelled.
    pub fn sleep(&self, duration: Duration) {
        let until = Instant::now() + duration;
        while !self.is_cancelled() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            thread::sleep(left.min(Duration::from_millis(100)));
        }
    }

    /// Fraction complete in `0.0..=1.0`, by bytes when known, else by items.
    pub fn ratio(&self) -> f64 {
        let total = self.total_bytes.load(Ordering::Relaxed);
        if total > 0 {
            return (self.done_bytes.load(Ordering::Relaxed) as f64 / total as f64).min(1.0);
        }
        let items = self.total_items.load(Ordering::Relaxed);
        if items > 0 {
            return (self.done_items.load(Ordering::Relaxed) as f64 / items as f64).min(1.0);
        }
        0.0
    }

    /// Smoothed bytes per second and, once the rate has settled, the time
    /// left. Each call samples the byte counter, so callers that draw
    /// regularly (the UI) keep the estimate current. `None` until bytes
    /// start moving.
    pub fn throughput(&self) -> Option<Throughput> {
        let done = self.done_bytes.load(Ordering::Relaxed);
        let total = self.total_bytes.load(Ordering::Relaxed);
        let mut meter = self.meter.lock().unwrap_or_else(|e| e.into_inner());
        meter.sample(Instant::now(), done, self.is_paused());
        let rate = meter.rate()?;
        let eta = (total > done).then(|| meter.eta(total - done)).flatten();
        Some(Throughput { bytes_per_sec: rate, eta })
    }
}

/// A job's current speed, from [`Progress::throughput`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Throughput {
    pub bytes_per_sec: f64,
    /// Time left at the current speed; `None` while the speed is still
    /// settling or nothing is moving.
    pub eta: Option<Duration>,
}

/// Exponentially weighted moving average of a byte counter's rate, so a
/// burst or a stall moves the estimate gradually instead of making it jump.
/// Time spent paused (or before any byte moved, such as while scanning) is
/// left out.
#[derive(Debug, Default, Clone)]
pub struct RateMeter {
    /// The previous sample: when, and the counter then.
    last: Option<(Instant, u64)>,
    /// Bytes per second.
    rate: f64,
    /// Running time covered by the average, in seconds.
    observed: f64,
}

impl RateMeter {
    /// Samples closer together than this are skipped: too noisy.
    const MIN_INTERVAL: f64 = 0.25;
    /// Time constant of the average: older speed fades with `e^(-t/TAU)`.
    const TAU: f64 = 3.0;
    /// How much running time the average needs before it predicts a finish.
    const SETTLED: f64 = 2.0;

    /// Records the counter's value `done` at `now`. A paused job breaks the
    /// series, so the pause does not read as a slow stretch.
    pub fn sample(&mut self, now: Instant, done: u64, paused: bool) {
        let Some((then, before)) = self.last.filter(|_| !paused && done > 0) else {
            self.last = (!paused).then_some((now, done));
            return;
        };
        let dt = now.saturating_duration_since(then).as_secs_f64();
        if dt < Self::MIN_INTERVAL {
            return;
        }
        let current = done.saturating_sub(before) as f64 / dt;
        let weight = if self.observed == 0.0 { 1.0 } else { 1.0 - (-dt / Self::TAU).exp() };
        self.rate += weight * (current - self.rate);
        self.observed += dt;
        self.last = Some((now, done));
    }

    /// Bytes per second, once there is a sample.
    pub fn rate(&self) -> Option<f64> {
        (self.observed > 0.0).then_some(self.rate)
    }

    /// Time to move `remaining` bytes at the current rate, when the rate
    /// has settled and the answer is plausible (under a week).
    pub fn eta(&self, remaining: u64) -> Option<Duration> {
        if self.observed < Self::SETTLED || self.rate < 1.0 {
            return None;
        }
        let secs = remaining as f64 / self.rate;
        (secs < 7.0 * 86_400.0).then(|| Duration::from_secs_f64(secs.ceil()))
    }
}

/// Marker error for user cancellation.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[derive(Debug)]
pub struct Job {
    pub id: u64,
    pub label: String,
    pub started: Instant,
    pub progress: Arc<Progress>,
    state: Mutex<JobState>,
}

impl Job {
    pub fn state(&self) -> JobState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_running(&self) -> bool {
        self.state() == JobState::Running
    }
}

/// Notification sent when a job finishes.
#[derive(Debug, Clone)]
pub struct JobFinished {
    pub id: u64,
    pub label: String,
    pub state: JobState,
}

/// Spawns jobs on worker threads and keeps a short history for the UI.
pub struct JobManager<E> {
    jobs: Vec<Arc<Job>>,
    next_id: u64,
    notify: Sender<E>,
    wrap: fn(JobFinished) -> E,
}

impl<E: Send + 'static> JobManager<E> {
    /// `wrap` turns a completion notice into the caller's event type.
    pub fn new(notify: Sender<E>, wrap: fn(JobFinished) -> E) -> Self {
        Self { jobs: Vec::new(), next_id: 1, notify, wrap }
    }

    pub fn spawn<F>(&mut self, label: impl Into<String>, work: F) -> u64
    where
        F: FnOnce(&Progress) -> Result<()> + Send + 'static,
    {
        let id = self.next_id;
        self.next_id += 1;
        let job = Arc::new(Job {
            id,
            label: label.into(),
            started: Instant::now(),
            progress: Arc::new(Progress::default()),
            state: Mutex::new(JobState::Running),
        });
        self.jobs.push(job.clone());
        self.prune();

        let notify = self.notify.clone();
        let wrap = self.wrap;
        thread::spawn(move || {
            let state = match work(&job.progress) {
                Ok(()) => JobState::Done,
                Err(e) if e.is::<Cancelled>() => JobState::Cancelled,
                Err(e) => JobState::Failed(format!("{e:#}")),
            };
            *job.state.lock().unwrap_or_else(|e| e.into_inner()) = state.clone();
            let _ = notify.send(wrap(JobFinished { id: job.id, label: job.label.clone(), state }));
        });
        id
    }

    pub fn jobs(&self) -> &[Arc<Job>] {
        &self.jobs
    }

    pub fn running(&self) -> usize {
        self.jobs.iter().filter(|j| j.is_running()).count()
    }

    /// Cancels the most recently started running job.
    pub fn cancel_latest(&self) -> bool {
        match self.jobs.iter().rev().find(|j| j.is_running()) {
            Some(job) => {
                job.progress.cancel();
                true
            }
            None => false,
        }
    }

    /// Pauses the most recently started running job, or resumes it if it
    /// is paused. Returns the job.
    pub fn toggle_pause_latest(&self) -> Option<&Arc<Job>> {
        let job = self.jobs.iter().rev().find(|j| j.is_running())?;
        job.progress.set_paused(!job.progress.is_paused());
        Some(job)
    }

    pub fn cancel_all(&self) {
        self.jobs.iter().for_each(|j| j.progress.cancel());
    }

    fn prune(&mut self) {
        const KEEP_FINISHED: usize = 20;
        let finished = self.jobs.iter().filter(|j| !j.is_running()).count();
        let mut excess = finished.saturating_sub(KEEP_FINISHED);
        self.jobs.retain(|j| {
            if excess > 0 && !j.is_running() {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn jobs_report_completion() {
        let (tx, rx) = mpsc::channel();
        let mut jobs = JobManager::new(tx, |f| f);
        jobs.spawn("ok", |p| {
            p.total_items.store(1, Ordering::Relaxed);
            p.item_done();
            Ok(())
        });
        jobs.spawn("fail", |_| bail!("boom"));
        let mut states: Vec<_> = (0..2).map(|_| rx.recv().unwrap()).collect();
        states.sort_by_key(|f| f.id);
        assert_eq!(states[0].state, JobState::Done);
        assert_eq!(states[1].state, JobState::Failed("boom".into()));
    }

    #[test]
    fn paused_jobs_wait_and_can_be_cancelled() {
        let p = Arc::new(Progress::default());
        p.set_paused(true);
        let worker = {
            let p = p.clone();
            thread::spawn(move || p.check().is_err())
        };
        thread::sleep(Duration::from_millis(250));
        assert!(!worker.is_finished(), "a paused job waits");
        p.cancel();
        assert!(worker.join().unwrap(), "cancelling a paused job stops it");
    }

    #[test]
    fn rate_is_smoothed_and_predicts_the_finish() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut m = RateMeter::default();
        // Nothing moved yet (scanning): no rate.
        m.sample(at(0), 0, false);
        m.sample(at(5_000), 0, false);
        assert_eq!(m.rate(), None);
        // 10 MB/s from then on: the scan time does not drag it down.
        for i in 1..=10u64 {
            m.sample(at(5_000 + i * 500), i * 5_000_000, false);
        }
        assert!((m.rate().unwrap() - 10e6).abs() < 1.0);
        assert_eq!(m.eta(50_000_000), Some(Duration::from_secs(5)));
        // One stalled half second only dents the average.
        m.sample(at(10_500), 50_000_000, false);
        let dented = m.rate().unwrap();
        assert!(dented > 8e6 && dented < 10e6, "{dented}");
    }

    #[test]
    fn rate_ignores_paused_time_and_needs_time_to_settle() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut m = RateMeter::default();
        m.sample(at(0), 1, false);
        m.sample(at(1_000), 1_000_001, false);
        assert_eq!(m.rate(), Some(1e6));
        assert_eq!(m.eta(1_000), None, "one second is too little to predict a finish");
        // Paused for a minute: the bytes counter stands still.
        m.sample(at(1_500), 1_500_001, true);
        m.sample(at(61_500), 1_500_001, true);
        m.sample(at(62_000), 1_500_001, false);
        m.sample(at(63_000), 2_500_001, false);
        assert!((m.rate().unwrap() - 1e6).abs() < 1.0, "the pause does not count as slow time");
        assert_eq!(m.eta(3_000_000), Some(Duration::from_secs(3)));
        // Samples too close together are skipped.
        m.sample(at(63_010), 9_000_000, false);
        assert!((m.rate().unwrap() - 1e6).abs() < 1.0);
    }
}
