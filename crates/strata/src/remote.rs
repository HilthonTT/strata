//! Driving a running strata from outside (`strata --remote`).
//!
//! Each instance listens on a Unix socket named after its pid, in
//! `$XDG_RUNTIME_DIR/strata` or else a private folder in the temp dir. A
//! client writes one request per line and reads one JSON reply per line.
//! A request is `{"command": "cd ~"}`, `{"query": "state"}` or a bare
//! command line, so `socat` and `nc -U` work too.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Set in the environment of programs strata starts, so `strata --remote`
/// run from them drives the strata that started them.
pub const INSTANCE_ENV: &str = "STRATA_INSTANCE";

/// Longest request line accepted, in bytes.
#[cfg_attr(not(unix), allow(dead_code))]
const MAX_LINE: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Request {
    /// A command-palette line, with or without the leading `:`.
    Command(String),
    /// `state`: the focused directory, hovered item and marked items.
    Query(String),
}

/// The answer to one request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    /// Notifications the command raised, errors included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
}

impl Reply {
    pub fn failed(error: impl Into<String>) -> Self {
        Reply { ok: false, error: Some(error.into()), ..Default::default() }
    }
}

/// Reads one request line: JSON when it starts with `{`, else a command.
pub fn parse_request(line: &str) -> Result<Request, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty request".into());
    }
    if line.starts_with('{') {
        return serde_json::from_str(line).map_err(|e| format!("bad request: {e}"));
    }
    Ok(Request::Command(line.to_string()))
}

/// A running strata that can be driven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub pid: u32,
    pub socket: PathBuf,
    pub started: SystemTime,
}

/// The instance asked for by pid, else the most recently started one.
/// `instances` is newest first, as [`instances`] returns it.
pub fn pick(instances: &[Instance], pid: Option<u32>) -> Result<Instance, String> {
    match pid {
        Some(pid) => {
            instances.iter().find(|i| i.pid == pid).cloned().ok_or(format!("no strata instance with pid {pid}"))
        }
        None => instances.first().cloned().ok_or_else(|| "no running strata instance".into()),
    }
}

/// The pid in `--instance`, else in `$STRATA_INSTANCE`.
pub fn wanted_pid(flag: Option<u32>) -> Result<Option<u32>, String> {
    if flag.is_some() {
        return Ok(flag);
    }
    match std::env::var(INSTANCE_ENV) {
        Ok(v) if !v.trim().is_empty() => {
            v.trim().parse().map(Some).map_err(|_| format!("{INSTANCE_ENV}={v} is not a pid"))
        }
        _ => Ok(None),
    }
}

/// Sorts newest first, the higher pid winning a tie.
#[cfg_attr(not(unix), allow(dead_code))]
fn sort_newest_first(instances: &mut [Instance]) {
    instances.sort_by(|a, b| b.started.cmp(&a.started).then(b.pid.cmp(&a.pid)));
}

#[cfg(unix)]
pub use unix::{instances, send, Server};

#[cfg(not(unix))]
pub use unsupported::{instances, send, Server};

#[cfg(unix)]
mod unix {
    use std::fs::{self, DirBuilder};
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, SystemTime};

    use super::{parse_request, sort_newest_first, Instance, Reply, Request, MAX_LINE};

    /// How long a client waits for each reply.
    const REPLY_TIMEOUT: Duration = Duration::from_secs(35);
    /// How long the server waits for the next request on a connection.
    const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

    /// The folder holding the sockets, made private to this user.
    fn socket_dir() -> io::Result<PathBuf> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join("strata"))
            .filter(|d| fits_socket_path(d));
        let dir = runtime.unwrap_or_else(|| {
            let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
            std::env::temp_dir().join(format!("strata-{}", if user.is_empty() { "user" } else { &user }))
        });
        ensure_private_dir(&dir)?;
        Ok(dir)
    }

    /// Socket paths are limited to about 104 bytes (`sun_path`).
    pub(super) fn fits_socket_path(dir: &Path) -> bool {
        dir.join("4294967295.sock").as_os_str().len() < 100
    }

    /// Creates `dir` readable only by its owner, or checks that it is.
    pub(super) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
        match DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
        // An existing folder must not be a symlink or open to others, or
        // another user could plant or read sockets in it.
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must be a folder only you can open (chmod 700)", dir.display()),
            ));
        }
        Ok(())
    }

    /// The running instances, newest first. Sockets left by instances that
    /// died are removed.
    pub fn instances() -> io::Result<Vec<Instance>> {
        instances_in(&socket_dir()?)
    }

    pub(super) fn instances_in(dir: &Path) -> io::Result<Vec<Instance>> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            let Some(pid) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".sock"))
                .and_then(|p| p.parse::<u32>().ok())
            else {
                continue;
            };
            match UnixStream::connect(&path) {
                Ok(_) => {
                    let started = entry.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                    found.push(Instance { pid, socket: path, started });
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound) => {
                    let _ = fs::remove_file(&path);
                }
                Err(_) => {}
            }
        }
        sort_newest_first(&mut found);
        Ok(found)
    }

    /// Sends `requests` to `instance` and returns a reply to each.
    pub fn send(instance: &Instance, requests: &[Request]) -> io::Result<Vec<Reply>> {
        let stream = UnixStream::connect(&instance.socket)?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        let mut replies = Vec::with_capacity(requests.len());
        for request in requests {
            let mut line = serde_json::to_string(request).map_err(io::Error::other)?;
            line.push('\n');
            writer.write_all(line.as_bytes())?;
            let mut answer = String::new();
            if reader.read_line(&mut answer)? == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "strata closed the connection"));
            }
            replies.push(serde_json::from_str(&answer).map_err(io::Error::other)?);
        }
        Ok(replies)
    }

    type Handler = Arc<dyn Fn(Request) -> Reply + Send + Sync>;

    /// The socket this instance listens on. Dropping it removes the socket.
    pub struct Server {
        path: PathBuf,
    }

    impl Server {
        /// Listens for this process, answering each request with `handler`
        /// on a background thread.
        pub fn start(handler: impl Fn(Request) -> Reply + Send + Sync + 'static) -> io::Result<Server> {
            Self::bind_in(&socket_dir()?, std::process::id(), Arc::new(handler))
        }

        pub(super) fn bind_in(dir: &Path, pid: u32, handler: Handler) -> io::Result<Server> {
            let path = dir.join(format!("{pid}.sock"));
            // Left by an earlier process with the same pid.
            let _ = fs::remove_file(&path);
            let listener = UnixListener::bind(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let handler = handler.clone();
                    thread::spawn(move || serve(stream, &*handler));
                }
            });
            Ok(Server { path })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    /// Answers requests on one connection until it closes or goes quiet.
    fn serve(stream: UnixStream, handler: &(dyn Fn(Request) -> Reply + Send + Sync)) {
        let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
        let Ok(reader) = stream.try_clone() else { return };
        let mut reader = BufReader::new(reader);
        let mut writer = stream;
        loop {
            let mut line = String::new();
            let reply = match (&mut reader).take(MAX_LINE).read_line(&mut line) {
                Ok(0) => return,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return,
                Err(e) => Reply::failed(format!("bad request: {e}")),
                Ok(_) if !line.ends_with('\n') && line.len() as u64 >= MAX_LINE => {
                    let _ = write_reply(&mut writer, &Reply::failed("request too long"));
                    return;
                }
                Ok(_) => match parse_request(&line) {
                    Ok(request) => handler(request),
                    Err(e) => Reply::failed(e),
                },
            };
            if write_reply(&mut writer, &reply).is_err() {
                return;
            }
        }
    }

    fn write_reply(writer: &mut UnixStream, reply: &Reply) -> io::Result<()> {
        let mut text = serde_json::to_string(reply).map_err(io::Error::other)?;
        text.push('\n');
        writer.write_all(text.as_bytes())
    }
}

#[cfg(not(unix))]
mod unsupported {
    use std::io;

    use super::{Instance, Reply, Request};

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "remote control needs Unix sockets and is not available on Windows yet",
        )
    }

    pub fn instances() -> io::Result<Vec<Instance>> {
        Err(unsupported())
    }

    pub fn send(_: &Instance, _: &[Request]) -> io::Result<Vec<Reply>> {
        Err(unsupported())
    }

    pub struct Server;

    impl Server {
        pub fn start(_: impl Fn(Request) -> Reply + Send + Sync + 'static) -> io::Result<Server> {
            Err(unsupported())
        }
    }
}

/// Runs the `--remote`, `--remote-query` and `--list-instances` client and
/// returns the exit code.
pub fn run_client(commands: &[String], query: bool, list: bool, pid: Option<u32>) -> i32 {
    match client(commands, query, list, pid) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("strata: {e}");
            1
        }
    }
}

fn client(commands: &[String], query: bool, list: bool, pid: Option<u32>) -> Result<i32, String> {
    let found = instances().map_err(|e| e.to_string())?;
    if list {
        for instance in &found {
            let cwd = send(instance, &[Request::Query("state".into())])
                .ok()
                .and_then(|r| r.into_iter().next()?.state)
                .and_then(|s| s.get("cwd")?.as_str().map(str::to_string))
                .unwrap_or_default();
            println!("{}\t{cwd}", instance.pid);
        }
        return Ok(0);
    }
    let target = pick(&found, wanted_pid(pid)?)?;
    let mut requests: Vec<Request> = commands.iter().map(|c| Request::Command(c.clone())).collect();
    if query {
        requests.push(Request::Query("state".into()));
    }
    let replies = send(&target, &requests).map_err(|e| format!("strata {}: {e}", target.pid))?;
    let mut code = 0;
    for reply in replies {
        if let Some(state) = reply.state {
            println!("{}", serde_json::to_string_pretty(&state).unwrap_or_default());
        }
        if reply.ok {
            reply.messages.iter().for_each(|m| println!("{m}"));
        } else {
            eprintln!("strata: {}", reply.error.unwrap_or_else(|| "command failed".into()));
            code = 1;
        }
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn requests_parse_from_json_or_bare_lines() {
        assert_eq!(parse_request(":cd ~\n"), Ok(Request::Command(":cd ~".into())));
        assert_eq!(parse_request("select *.rs"), Ok(Request::Command("select *.rs".into())));
        assert_eq!(parse_request(r#"{"command": "cd /tmp"}"#), Ok(Request::Command("cd /tmp".into())));
        assert_eq!(parse_request(r#"{"query":"state"}"#), Ok(Request::Query("state".into())));
        assert!(parse_request("  ").is_err());
        assert!(parse_request(r#"{"command": 3}"#).is_err());
        assert!(parse_request(r#"{"bogus": "x"}"#).is_err());
        assert!(parse_request("{not json").is_err());
    }

    #[test]
    fn replies_round_trip_and_stay_small() {
        let ok = Reply { ok: true, ..Default::default() };
        assert_eq!(serde_json::to_string(&ok).unwrap(), r#"{"ok":true}"#);
        let failed = Reply::failed("unknown command: nope");
        let text = serde_json::to_string(&failed).unwrap();
        assert_eq!(serde_json::from_str::<Reply>(&text).unwrap(), failed);
    }

    fn instance(pid: u32, secs: u64) -> Instance {
        Instance {
            pid,
            socket: PathBuf::from(format!("{pid}.sock")),
            started: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    #[test]
    fn picks_the_asked_pid_else_the_newest() {
        let mut found = vec![instance(10, 100), instance(20, 300), instance(30, 200)];
        sort_newest_first(&mut found);
        assert_eq!(found.iter().map(|i| i.pid).collect::<Vec<_>>(), [20, 30, 10]);
        assert_eq!(pick(&found, None).unwrap().pid, 20);
        assert_eq!(pick(&found, Some(10)).unwrap().pid, 10);
        assert_eq!(pick(&found, Some(99)).unwrap_err(), "no strata instance with pid 99");
        assert_eq!(pick(&[], None).unwrap_err(), "no running strata instance");
    }

    #[test]
    fn the_instance_flag_wins_over_the_environment() {
        assert_eq!(wanted_pid(Some(7)), Ok(Some(7)));
    }

    #[cfg(unix)]
    #[test]
    fn a_server_answers_and_stale_sockets_are_removed() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let handler = Arc::new(|request: Request| match request {
            Request::Command(line) if line == "fail" => Reply::failed("it failed"),
            Request::Command(line) => Reply { ok: true, messages: vec![line], ..Default::default() },
            Request::Query(_) => {
                Reply { ok: true, state: Some(serde_json::json!({"cwd": "/x"})), ..Default::default() }
            }
        });
        let server = unix::Server::bind_in(dir.path(), 4242, handler).unwrap();
        // A socket nobody listens on any more.
        drop(std::os::unix::net::UnixListener::bind(dir.path().join("77.sock")).unwrap());
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();

        let found = unix::instances_in(dir.path()).unwrap();
        assert_eq!(found.iter().map(|i| i.pid).collect::<Vec<_>>(), [4242]);
        assert!(!dir.path().join("77.sock").exists());

        let requests =
            [Request::Command("cd /".into()), Request::Command("fail".into()), Request::Query("state".into())];
        let replies = send(&found[0], &requests).unwrap();
        assert_eq!(replies[0].messages, ["cd /"]);
        assert_eq!(replies[1].error.as_deref(), Some("it failed"));
        assert_eq!(replies[2].state.as_ref().unwrap()["cwd"], "/x");

        drop(server);
        assert!(unix::instances_in(dir.path()).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn malformed_lines_get_an_error_reply() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let _server =
            unix::Server::bind_in(dir.path(), 1, Arc::new(|_| Reply { ok: true, ..Default::default() })).unwrap();
        let mut stream = std::os::unix::net::UnixStream::connect(dir.path().join("1.sock")).unwrap();
        stream.write_all(b"{oops\nquit\n").unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(!serde_json::from_str::<Reply>(&line).unwrap().ok);
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(serde_json::from_str::<Reply>(&line).unwrap().ok);
    }

    #[cfg(unix)]
    #[test]
    fn long_runtime_dirs_are_passed_over() {
        assert!(unix::fits_socket_path(std::path::Path::new("/run/user/1000/strata")));
        assert!(!unix::fits_socket_path(&PathBuf::from("/").join("x".repeat(90))));
    }

    #[cfg(unix)]
    #[test]
    fn shared_folders_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("strata");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = unix::ensure_private_dir(&shared).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
        unix::ensure_private_dir(&shared).unwrap();
        let fresh = dir.path().join("fresh");
        unix::ensure_private_dir(&fresh).unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
