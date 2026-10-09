//! NAS connections (SMB, NFS, SFTP): reachability probes, step-by-step
//! diagnostics and the platform commands that mount and unmount shares.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Smb,
    Nfs,
    Sftp,
}

impl Protocol {
    pub fn default_port(self) -> u16 {
        match self {
            Self::Smb => 445,
            Self::Nfs => 2049,
            Self::Sftp => 22,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smb => "smb",
            Self::Nfs => "nfs",
            Self::Sftp => "sftp",
        }
    }
}

/// A saved connection, as written in `config.toml` under `[[connections]]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub protocol: Protocol,
    /// IPv4, IPv6 or hostname (`192.168.0.10`, `fd00::5`, `nas.local`).
    pub host: String,
    /// Share name (SMB), export path (NFS) or start directory (SFTP).
    #[serde(default)]
    pub share: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// Where SMB/NFS shares get mounted. Defaults to `~/mnt/<name>`.
    #[serde(default)]
    pub mount_point: Option<PathBuf>,
    /// SSH private key for SFTP. The SSH agent and `~/.ssh` keys are tried otherwise.
    #[serde(default)]
    pub identity_file: Option<PathBuf>,
    /// SMB workgroup / domain (defaults to `WORKGROUP`).
    #[serde(default)]
    pub domain: Option<String>,
    /// Probe and mount automatically when strata starts.
    #[serde(default)]
    pub auto_connect: bool,
}

impl Connection {
    /// Parses `smb://user@host/share`, `nfs://host/export` or
    /// `sftp://user@host:2222/path`.
    pub fn from_url(name: &str, url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let protocol = match scheme.to_ascii_lowercase().as_str() {
            "smb" | "cifs" => Protocol::Smb,
            "nfs" => Protocol::Nfs,
            "sftp" | "ssh" => Protocol::Sftp,
            _ => return None,
        };
        let (authority, path) =
            rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, String::new()));
        let (user, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, authority),
        };
        let (host, port) = split_host_port(hostport);
        if host.is_empty() {
            return None;
        }
        let share = match protocol {
            Protocol::Smb => path.trim_matches('/').to_string(),
            _ => path,
        };
        Some(Self {
            name: if name.is_empty() { host.clone() } else { name.to_string() },
            protocol,
            host,
            share,
            user,
            port,
            mount_point: None,
            identity_file: None,
            domain: None,
            auto_connect: false,
        })
    }

    pub fn port(&self) -> u16 {
        self.port.unwrap_or(self.protocol.default_port())
    }

    pub fn url(&self) -> String {
        let user = self.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let share = self.share.trim_start_matches('/');
        format!("{}://{user}{}{port}/{share}", self.protocol.as_str(), bracket_ipv6(&self.host))
    }

    pub fn mount_point(&self) -> PathBuf {
        self.mount_point.as_ref().map(|p| crate::util::expand_tilde(&p.to_string_lossy())).unwrap_or_else(|| {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("mnt").join(sanitize(&self.name))
        })
    }

    /// Where the share is currently mounted, if it is.
    pub fn mounted_at(&self) -> Option<PathBuf> {
        if self.protocol == Protocol::Sftp {
            return None;
        }
        if let Some(gvfs) = self.gvfs_path().filter(|p| p.exists()) {
            return Some(gvfs);
        }
        let mounts = read_mounts();
        let host = self.host.to_ascii_lowercase();
        let share = self.share.trim_matches('/').to_ascii_lowercase();
        mounts.into_iter().find_map(|(source, target)| {
            let s = source.to_ascii_lowercase().replace('\\', "/");
            let matches_host = s.contains(&host);
            let matches_share = share.is_empty() || s.trim_end_matches('/').ends_with(&share);
            (matches_host && matches_share).then_some(target)
        })
    }

    /// GVFS (GNOME `gio mount`) location for SMB shares on Linux.
    fn gvfs_path(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        if self.protocol == Protocol::Smb {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata("/proc/self").ok()?.uid();
            let share = self.share.trim_matches('/').to_ascii_lowercase();
            return Some(PathBuf::from(format!(
                "/run/user/{uid}/gvfs/smb-share:server={},share={share}",
                self.host.to_ascii_lowercase()
            )));
        }
        None
    }

    /// Command that mounts the share. It runs with the terminal handed
    /// over, so `sudo` or the server can prompt for a password.
    pub fn mount_command(&self) -> Option<Vec<String>> {
        self.mount_plan(None).map(|p| p.argv)
    }

    /// How to mount the share, using `password` (from the keychain) when
    /// given. Passwords go through stdin or a private credentials file
    /// where the platform tool allows it, rather than the command line.
    pub fn mount_plan(&self, password: Option<&str>) -> Option<MountPlan> {
        let mp = self.mount_point().to_string_lossy().into_owned();
        let user = self.user.clone().unwrap_or_else(whoami);
        let domain = self.domain.clone().unwrap_or_else(|| "WORKGROUP".into());
        let share = self.share.trim_matches('/');
        let mut plan = MountPlan::default();
        plan.argv = match self.protocol {
            Protocol::Sftp => return None,
            Protocol::Smb if cfg!(target_os = "windows") => {
                let mut argv =
                    vec!["net".into(), "use".into(), format!(r"\\{}\{}", self.host, share.replace('/', "\\"))];
                // `net use` has no stdin option; `*` makes it prompt instead.
                argv.push(password.map(str::to_string).unwrap_or_else(|| "*".into()));
                argv.push(format!("/user:{user}"));
                argv
            }
            Protocol::Smb if cfg!(target_os = "macos") => {
                let auth = match password {
                    Some(pw) => format!("{}:{}", percent_encode(&user), percent_encode(pw)),
                    None => percent_encode(&user),
                };
                vec!["mount_smbfs".into(), format!("//{auth}@{}/{share}", self.host), mp]
            }
            Protocol::Smb if which("gio") => {
                // gio asks for user, domain and password on stdin.
                plan.stdin = password.map(|pw| format!("{user}\n{domain}\n{pw}\n"));
                vec!["gio".into(), "mount".into(), self.url()]
            }
            Protocol::Smb => {
                let options = match password {
                    Some(pw) => {
                        plan.credentials = Some(format!("username={user}\npassword={pw}\ndomain={domain}\n"));
                        format!("credentials={CREDENTIALS_FILE},uid={},gid={}", id_of("-u"), id_of("-g"))
                    }
                    None => format!("username={user},uid={},gid={}", id_of("-u"), id_of("-g")),
                };
                vec![
                    "sudo".into(),
                    "mount".into(),
                    "-t".into(),
                    "cifs".into(),
                    format!("//{}/{share}", self.host),
                    mp,
                    "-o".into(),
                    options,
                ]
            }
            Protocol::Nfs if cfg!(target_os = "windows") => {
                vec!["mount".into(), format!(r"\\{}{}", self.host, self.share.replace('/', "\\")), "*".into()]
            }
            Protocol::Nfs => {
                vec![
                    "sudo".into(),
                    "mount".into(),
                    "-t".into(),
                    "nfs".into(),
                    format!("{}:{}", self.host, self.share),
                    mp,
                ]
            }
        };
        Some(plan)
    }

    pub fn unmount_command(&self, mounted_at: &Path) -> Vec<String> {
        let target = mounted_at.to_string_lossy().into_owned();
        if cfg!(target_os = "windows") {
            return vec!["net".into(), "use".into(), target, "/delete".into()];
        }
        if mounted_at.to_string_lossy().contains("/gvfs/") {
            return vec!["gio".into(), "mount".into(), "-u".into(), self.url()];
        }
        if cfg!(target_os = "macos") {
            return vec!["umount".into(), target];
        }
        vec!["sudo".into(), "umount".into(), target]
    }

    /// Whether mounting needs a local directory to exist first.
    pub fn needs_mount_dir(&self) -> bool {
        match self.protocol {
            Protocol::Sftp => false,
            Protocol::Smb => !(cfg!(target_os = "windows") || (cfg!(target_os = "linux") && which("gio"))),
            Protocol::Nfs => !cfg!(target_os = "windows"),
        }
    }
}

/// Placeholder in [`MountPlan::argv`] for the path of the credentials file.
pub const CREDENTIALS_FILE: &str = "{credentials-file}";

/// A mount command plus how to hand it the password.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MountPlan {
    pub argv: Vec<String>,
    /// Text to write to the command's stdin.
    pub stdin: Option<String>,
    /// Contents of a private credentials file whose path replaces
    /// [`CREDENTIALS_FILE`] in `argv`.
    pub credentials: Option<String>,
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Result of a quick TCP probe.
#[derive(Debug, Clone, PartialEq)]
pub enum Reachability {
    Unknown,
    Up { latency: Duration, port: u16 },
    Down(String),
}

/// Resolves the host and tries the given ports in order.
pub fn probe(host: &str, ports: &[u16], timeout: Duration) -> Reachability {
    let mut last_err = String::from("no ports to try");
    for &port in ports {
        let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
            Ok(a) => a.collect(),
            Err(e) => return Reachability::Down(format!("cannot resolve {host}: {e}")),
        };
        for addr in addrs {
            let start = Instant::now();
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(_) => return Reachability::Up { latency: start.elapsed(), port },
                Err(e) => last_err = format!("port {port}: {e}"),
            }
        }
    }
    Reachability::Down(last_err)
}

/// Ports worth probing for a connection; SMB falls back to NetBIOS (139).
pub fn probe_ports(conn: &Connection) -> Vec<u16> {
    match (conn.protocol, conn.port) {
        (Protocol::Smb, None) => vec![445, 139],
        _ => vec![conn.port()],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Ok,
    Warn,
    Fail,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: &'static str,
    pub status: StepStatus,
    pub detail: String,
}

/// Walks the chain a mount takes — name, reachability, mount point, mount
/// — and reports each step. Steps that cannot run are marked skipped rather
/// than guessed at.
pub fn diagnose(conn: &Connection) -> Vec<Step> {
    let mut steps = Vec::new();
    let resolved = (conn.host.as_str(), conn.port()).to_socket_addrs().map(|a| a.collect::<Vec<_>>());
    let resolved_ok = match &resolved {
        Ok(addrs) if !addrs.is_empty() => {
            let list: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).collect();
            steps.push(Step { name: "Server name", status: StepStatus::Ok, detail: list.join(", ") });
            true
        }
        Ok(_) => {
            steps.push(Step { name: "Server name", status: StepStatus::Fail, detail: "no addresses".into() });
            false
        }
        Err(e) => {
            steps.push(Step { name: "Server name", status: StepStatus::Fail, detail: e.to_string() });
            false
        }
    };

    let reachable = if resolved_ok {
        let ports = probe_ports(conn);
        match probe(&conn.host, &ports, Duration::from_secs(3)) {
            Reachability::Up { latency, port } => {
                let status = if port == 139 { StepStatus::Warn } else { StepStatus::Ok };
                steps.push(Step {
                    name: "Reach server",
                    status,
                    detail: format!("port {port} answered in {} ms", latency.as_millis()),
                });
                true
            }
            other => {
                let detail = match other {
                    Reachability::Down(e) => e,
                    _ => "unknown".into(),
                };
                steps.push(Step { name: "Reach server", status: StepStatus::Fail, detail });
                false
            }
        }
    } else {
        steps.push(Step { name: "Reach server", status: StepStatus::Skipped, detail: "name did not resolve".into() });
        false
    };

    match conn.protocol {
        Protocol::Sftp => {
            steps.push(Step {
                name: "Credentials",
                status: if reachable { StepStatus::Ok } else { StepStatus::Skipped },
                detail: if reachable {
                    "checked when connecting (agent, key or password)".into()
                } else {
                    "server did not answer".into()
                },
            });
        }
        Protocol::Smb | Protocol::Nfs => {
            if conn.needs_mount_dir() {
                let mp = conn.mount_point();
                let (status, detail) = match std::fs::read_dir(&mp).map(|mut it| it.next().is_none()) {
                    Ok(true) => (StepStatus::Ok, format!("{} is empty", mp.display())),
                    Ok(false) if conn.mounted_at().is_some() => {
                        (StepStatus::Ok, format!("{} in use by this share", mp.display()))
                    }
                    Ok(false) => (StepStatus::Warn, format!("{} is not empty", mp.display())),
                    Err(_) => (StepStatus::Ok, format!("{} will be created", mp.display())),
                };
                steps.push(Step { name: "Mount point", status, detail });
            }
            let (status, detail) = match conn.mounted_at() {
                Some(at) => (StepStatus::Ok, format!("mounted at {}", at.display())),
                None if reachable => (StepStatus::Warn, "not mounted".into()),
                None => (StepStatus::Skipped, "server did not answer".into()),
            };
            steps.push(Step { name: "Share", status, detail });
        }
    }
    steps
}

fn split_host_port(s: &str) -> (String, Option<u16>) {
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            return (host.to_string(), tail.strip_prefix(':').and_then(|p| p.parse().ok()));
        }
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().ok()),
        _ => (s.to_string(), None),
    }
}

fn bracket_ipv6(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn whoami() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "guest".into())
}

fn id_of(flag: &str) -> String {
    Command::new("id")
        .arg(flag)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1000".into())
}

/// True when `program` is on the PATH.
pub fn which(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file() || candidate.with_extension("exe").is_file()
    })
}

/// `(source, target)` pairs of currently mounted filesystems.
fn read_mounts() -> Vec<(String, PathBuf)> {
    if let Ok(text) = std::fs::read_to_string("/proc/mounts") {
        return text
            .lines()
            .filter_map(|l| {
                let mut parts = l.split_whitespace();
                let src = parts.next()?;
                let target = parts.next()?.replace("\\040", " ");
                Some((src.to_string(), PathBuf::from(target)))
            })
            .collect();
    }
    // macOS / BSD: "//user@host/share on /Volumes/share (smbfs, ...)"
    Command::new("mount")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| {
                    let (src, rest) = l.split_once(" on ")?;
                    let target = rest.split(" (").next()?;
                    Some((src.to_string(), PathBuf::from(target)))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let c = Connection::from_url("nas", "smb://hans@192.168.0.10/media").unwrap();
        assert_eq!(c.protocol, Protocol::Smb);
        assert_eq!(c.host, "192.168.0.10");
        assert_eq!(c.share, "media");
        assert_eq!(c.user.as_deref(), Some("hans"));
        assert_eq!(c.port(), 445);

        let s = Connection::from_url("", "sftp://me@[fd00::5]:2222/volume1").unwrap();
        assert_eq!(s.host, "fd00::5");
        assert_eq!(s.port, Some(2222));
        assert_eq!(s.share, "/volume1");
        assert_eq!(s.name, "fd00::5");
        assert_eq!(s.url(), "sftp://me@[fd00::5]:2222/volume1");

        let n = Connection::from_url("x", "nfs://nas.local/export/data").unwrap();
        assert_eq!(n.share, "/export/data");
        assert!(Connection::from_url("x", "http://nope").is_none());
    }

    #[test]
    fn passwords_stay_off_the_command_line_on_linux() {
        let c = Connection::from_url("n", "smb://me@host/media").unwrap();
        let plan = c.mount_plan(Some("s3cret")).unwrap();
        if cfg!(target_os = "linux") {
            assert!(!plan.argv.iter().any(|a| a.contains("s3cret")));
            assert!(plan.stdin.is_some() || plan.credentials.is_some());
        }
        assert_eq!(percent_encode("p@ss w"), "p%40ss%20w");
    }

    #[test]
    fn smb_probes_netbios_fallback() {
        let c = Connection::from_url("n", "smb://host/s").unwrap();
        assert_eq!(probe_ports(&c), vec![445, 139]);
    }

    #[test]
    fn unresolvable_hosts_skip_later_steps() {
        let c = Connection::from_url("n", "smb://does-not-exist.invalid/share").unwrap();
        let steps = diagnose(&c);
        assert_eq!(steps[0].status, StepStatus::Fail);
        assert_eq!(steps[1].status, StepStatus::Skipped);
    }
}
