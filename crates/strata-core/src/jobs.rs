//! Background jobs (copy, move, delete...) with shared progress counters.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

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
    current: Mutex<String>,
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
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Bails out of a job loop when the user cancelled it.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!(Cancelled);
        }
        Ok(())
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
        Self {
            jobs: Vec::new(),
            next_id: 1,
            notify,
            wrap,
        }
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
            let _ = notify.send(wrap(JobFinished {
                id: job.id,
                label: job.label.clone(),
                state,
            }));
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
}
