//! `:sudo`: running a file operation that was refused for lack of
//! permission again as root.

use strata_core::elevate::{is_permission_error, Privileged};
use strata_core::VfsRef;

use super::external::{After, External, Wait};
use super::overlay::{Confirm, ConfirmState, Overlay};
use super::App;

impl App {
    /// Remembers how to run `job` as root, should it be refused. Only local
    /// operations qualify, and only on Unix.
    pub(super) fn offer_elevation(&mut self, job: u64, vfs: &[&VfsRef], plan: Privileged) {
        if cfg!(unix) && vfs.iter().all(|v| v.is_local()) {
            self.elevatable.insert(job, plan);
        }
    }

    /// Called when a job finishes. When it failed for lack of permission and
    /// can run as root, keeps it for `:sudo` and returns a hint to show.
    pub(super) fn elevation_on_finish(&mut self, job: u64, error: Option<&str>) -> Option<&'static str> {
        let plan = self.elevatable.remove(&job);
        if !error.is_some_and(is_permission_error) {
            return None;
        }
        let plan = match plan {
            Some(Privileged::Delete(paths)) => {
                // Items deleted before the refusal are gone already.
                Privileged::Delete(paths.into_iter().filter(|p| p.symlink_metadata().is_ok()).collect())
            }
            Some(plan) => plan,
            None => {
                let (transfer, finished) = self.retryable.get(&job)?;
                if !cfg!(unix) || !transfer.src.is_local() || !transfer.dst.is_local() {
                    return None;
                }
                Privileged::Transfer { mode: transfer.mode, items: transfer.remaining(finished) }
            }
        };
        if plan.is_empty() {
            return None;
        }
        self.refused = Some(plan);
        Some(" (:sudo or alt+s retries it as root)")
    }

    /// Asks before running the last refused operation as root.
    pub(super) fn retry_as_root(&mut self) {
        let Some(plan) = self.refused.clone() else {
            return self.info("nothing was refused for lack of permission");
        };
        let program = self.elevate_command().join(" ");
        let message = format!("Run as root with {program}: {}? Undo will not cover it.", plan.describe());
        self.overlay = Some(Overlay::Confirm(ConfirmState { message, action: Confirm::Elevate(plan) }));
    }

    pub(super) fn run_as_root(&mut self, plan: Privileged) {
        self.refused = None;
        let argv = plan.argv(&self.elevate_command());
        // Stay in the terminal on failure so the error (or a wrong password)
        // can be read.
        self.queue_external(External::Run { argv, cwd: None, wait: Wait::PauseOnFailure, after: After::Reload });
    }

    fn elevate_command(&self) -> Vec<String> {
        let configured = strata_core::util::split_command(&self.config.general.elevate);
        if configured.is_empty() {
            vec!["sudo".into()]
        } else {
            configured
        }
    }
}
