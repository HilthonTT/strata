//! Commands from outside: `strata --command` at startup and requests from
//! `strata --remote`.

use std::sync::mpsc;
use std::time::Duration;

use strata_plugin::Level;

use super::App;
use crate::event::AppEvent;
use crate::remote::{Reply, Request, Server};

/// How long a remote request waits for the UI, e.g. while an editor runs.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

impl App {
    /// Starts listening for `strata --remote`, unless turned off.
    pub(super) fn start_remote_control(&mut self) {
        if !self.config.general.remote_control {
            return;
        }
        let tx = self.tx.clone();
        let started = Server::start(move |request| {
            let (reply_tx, reply_rx) = mpsc::channel();
            let ask = AppEvent::Then(Box::new(move |app: &mut App| {
                let _ = reply_tx.send(app.answer(request));
            }));
            if tx.send(ask).is_err() {
                return Reply::failed("strata is quitting");
            }
            reply_rx.recv_timeout(ANSWER_TIMEOUT).unwrap_or_else(|_| Reply::failed("strata is busy, try again"))
        });
        match started {
            Ok(server) => self.remote_server = Some(server),
            // Windows: there is nothing to warn about at every start.
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {}
            Err(e) => self.notify(format!("remote control: {e}"), Level::Warn),
        }
    }

    /// Runs the `--command` lines, in order.
    pub(super) fn run_startup_commands(&mut self) {
        for line in std::mem::take(&mut self.startup_commands) {
            self.run_command(&line);
        }
    }

    pub(super) fn answer(&mut self, request: Request) -> Reply {
        match request {
            Request::Command(line) => {
                let before = self.notified;
                self.run_command(&line);
                let raised = ((self.notified - before) as usize).min(self.notifications.len());
                let raised = &self.notifications[self.notifications.len() - raised..];
                let errors: Vec<&str> =
                    raised.iter().filter(|n| n.level == Level::Error).map(|n| n.message.as_str()).collect();
                Reply {
                    ok: errors.is_empty(),
                    messages: raised.iter().map(|n| n.message.clone()).collect(),
                    error: (!errors.is_empty()).then(|| errors.join("; ")),
                    state: None,
                }
            }
            Request::Query(what) if what == "state" => {
                Reply { ok: true, state: Some(self.state_json()), ..Default::default() }
            }
            Request::Query(what) => Reply::failed(format!("unknown query '{what}' (try 'state')")),
        }
    }

    /// The focused directory, hovered item and marked items.
    fn state_json(&self) -> serde_json::Value {
        let p = self.panel();
        let mut marked: Vec<_> = p.marked.iter().map(|m| m.to_string_lossy().into_owned()).collect();
        marked.sort();
        serde_json::json!({
            "pid": std::process::id(),
            "cwd": p.cwd.to_string_lossy(),
            "local": p.vfs.is_local(),
            "hovered": p.hovered().map(|e| e.path.to_string_lossy().into_owned()),
            "marked": marked,
            "panel": self.active,
            "panels": self.panels.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    fn app(dir: &std::path::Path) -> App {
        let options = crate::app::StartOptions {
            paths: vec![dir.to_path_buf()],
            config_path: dir.join("config.toml"),
            load_plugins: false,
            chooser: false,
            layout: None,
            commands: vec![],
        };
        App::new(strata_config::Config::default(), options, ratatui_image::picker::Picker::halfblocks()).unwrap()
    }

    #[test]
    fn commands_report_their_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let reply = app.answer(Request::Command(":no-such-command".into()));
        assert!(!reply.ok);
        assert_eq!(reply.error.as_deref(), Some("unknown command: no-such-command"));

        let reply = app.answer(Request::Command("mkdir sub".into()));
        assert!(reply.ok, "{reply:?}");
        assert!(dir.path().join("sub").is_dir());
    }

    #[test]
    fn state_names_the_focused_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let state = app.answer(Request::Query("state".into())).state.unwrap();
        assert_eq!(state["cwd"], dir.path().to_string_lossy().as_ref());
        assert_eq!(state["marked"], serde_json::json!([]));
        assert!(!app.answer(Request::Query("nope".into())).ok);
    }

    #[test]
    fn startup_commands_run_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        app.startup_commands = vec!["mkdir a".into(), "cd a".into(), ":mkdir b".into()];
        app.run_startup_commands();
        assert!(dir.path().join("a/b").is_dir());
        assert!(app.startup_commands.is_empty());
    }
}
