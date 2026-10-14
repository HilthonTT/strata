//! NAS and remote connections (SMB, NFS, SFTP, S3, WebDAV, FTP, rclone
//! remotes): reachability probes, step-by-step diagnostics, the platform
//! commands that mount and unmount shares and the rclone remotes that
//! browse the rest in-app.

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
    S3,
    Webdav,
    Ftp,
    /// A remote set up with `rclone config`; `host` is its name.
    Rclone,
}

impl Protocol {
    pub fn default_port(self) -> u16 {
        match self {
            Self::Smb => 445,
            Self::Nfs => 2049,
            Self::Sftp => 22,
            Self::S3 | Self::Webdav => 443,
            Self::Ftp => 21,
            Self::Rclone => 0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smb => "smb",
            Self::Nfs => "nfs",
            Self::Sftp => "sftp",
            Self::S3 => "s3",
            Self::Webdav => "webdav",
            Self::Ftp => "ftp",
            Self::Rclone => "rclone",
        }
    }

    /// Browsed inside strata rather than mounted on the local disk.
    pub fn in_app(self) -> bool {
        !matches!(self, Self::Smb | Self::Nfs)
    }

    /// Browsed through the `rclone` tool.
    pub fn uses_rclone(self) -> bool {
        matches!(self, Self::S3 | Self::Webdav | Self::Ftp | Self::Rclone)
    }
}

/// S3 endpoint used when a connection names none.
pub const AWS_S3_HOST: &str = "s3.amazonaws.com";

/// A saved connection, as written in `config.toml` under `[[connections]]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub protocol: Protocol,
    /// IPv4, IPv6 or hostname (`192.168.0.10`, `fd00::5`, `nas.local`), the
    /// S3 endpoint (`s3.amazonaws.com` by default) or the rclone remote name.
    pub host: String,
    /// Share name (SMB), export path (NFS), start directory (SFTP, FTP,
    /// rclone), `/bucket/prefix` (S3) or the server path of the WebDAV root.
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
    /// Encrypt the connection. Defaults to on for S3 and WebDAV and off for
    /// FTP (`ftps://` turns it on, `dav://` and `s3+http://` turn it off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<bool>,
}

impl Connection {
    /// Parses `smb://user@host/share`, `nfs://host/export`,
    /// `sftp://user@host:2222/path`, `s3://bucket/prefix`,
    /// `s3+http://minio.local:9000/bucket`, `davs://user@host/remote.php/dav`,
    /// `ftp://user@host/pub` or `rclone://remote/path`.
    pub fn from_url(name: &str, url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let (protocol, tls) = match scheme.to_ascii_lowercase().as_str() {
            "smb" | "cifs" => (Protocol::Smb, None),
            "nfs" => (Protocol::Nfs, None),
            "sftp" | "ssh" => (Protocol::Sftp, None),
            "s3" => {
                // The bucket comes first; the endpoint is AWS.
                let path = rest.trim_matches('/');
                if path.is_empty() {
                    return None;
                }
                let bucket = path.split('/').next().unwrap_or(path);
                let mut conn = Self::new(if name.is_empty() { bucket } else { name }, Protocol::S3, AWS_S3_HOST);
                conn.share = format!("/{path}");
                return Some(conn);
            }
            "s3+https" => (Protocol::S3, None),
            "s3+http" => (Protocol::S3, Some(false)),
            "webdav" | "webdavs" | "davs" => (Protocol::Webdav, None),
            "dav" => (Protocol::Webdav, Some(false)),
            "ftp" => (Protocol::Ftp, None),
            "ftps" => (Protocol::Ftp, Some(true)),
            "rclone" => (Protocol::Rclone, None),
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
        let mut conn = Self::new(if name.is_empty() { &host } else { name }, protocol, &host);
        conn.share = share;
        conn.user = user;
        conn.port = port;
        conn.tls = tls;
        Some(conn)
    }

    pub(crate) fn new(name: &str, protocol: Protocol, host: &str) -> Self {
        Self {
            name: name.to_string(),
            protocol,
            host: host.to_string(),
            share: String::new(),
            user: None,
            port: None,
            mount_point: None,
            identity_file: None,
            domain: None,
            auto_connect: false,
            tls: None,
        }
    }

    pub fn port(&self) -> u16 {
        if let Some(port) = self.port {
            return port;
        }
        match self.protocol {
            Protocol::S3 | Protocol::Webdav if !self.tls() => 80,
            Protocol::Ftp if self.tls() => 990,
            other => other.default_port(),
        }
    }

    /// Whether the connection is encrypted (S3, WebDAV and FTP only).
    pub fn tls(&self) -> bool {
        self.tls.unwrap_or(self.protocol != Protocol::Ftp)
    }

    pub fn url(&self) -> String {
        let user = self.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let share = self.share.trim_start_matches('/');
        let scheme = match self.protocol {
            Protocol::S3 if self.host == AWS_S3_HOST && self.port.is_none() && self.tls() => {
                return format!("s3://{share}");
            }
            Protocol::S3 if self.tls() => "s3+https",
            Protocol::S3 => "s3+http",
            Protocol::Webdav if self.tls() => "davs",
            Protocol::Webdav => "dav",
            Protocol::Ftp if self.tls() => "ftps",
            other => other.as_str(),
        };
        format!("{scheme}://{user}{}{port}/{share}", bracket_ipv6(&self.host))
    }

    /// The rclone remote that reaches this connection, and the environment
    /// that carries its secrets. `password` comes from the keychain or a
    /// prompt; it is scrambled with `rclone obscure` where rclone wants that.
    pub fn rclone_remote(&self, password: Option<&str>) -> anyhow::Result<RcloneRemote> {
        use crate::vfs::{rclone_obscure, rclone_quote as q};
        let mut env = Vec::new();
        let host = bracket_ipv6(&self.host);
        let http = if self.tls() { "https" } else { "http" };
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let start = if self.share.is_empty() { "/".to_string() } else { format!("/{}", self.share.trim_matches('/')) };
        let (remote, home) = match self.protocol {
            Protocol::S3 => {
                let mut remote = String::from(":s3");
                if self.host == AWS_S3_HOST && self.port.is_none() {
                    remote.push_str(",provider=AWS");
                } else {
                    remote.push_str(&format!(",provider=Other,endpoint={}", q(&format!("{http}://{host}{port}"))));
                }
                match (&self.user, password) {
                    (Some(key), Some(secret)) => {
                        remote.push_str(&format!(",access_key_id={}", q(key)));
                        env.push(("RCLONE_S3_SECRET_ACCESS_KEY".into(), secret.to_string()));
                    }
                    (Some(key), None) => anyhow::bail!("authentication failed: {key} needs a secret key"),
                    // AWS_* variables or ~/.aws profiles.
                    (None, _) => {
                        remote.push_str(",env_auth=true");
                        // Asking the EC2 metadata service for a role stalls
                        // for seconds anywhere but EC2; it can be turned back on.
                        if std::env::var_os("AWS_EC2_METADATA_DISABLED").is_none() {
                            env.push(("AWS_EC2_METADATA_DISABLED".into(), "true".into()));
                        }
                    }
                }
                (format!("{remote}:"), start)
            }
            Protocol::Webdav => {
                let url = format!("{http}://{host}{port}{}", start.trim_end_matches('/'));
                let mut remote = format!(":webdav,url={}", q(&url));
                if let Some(user) = &self.user {
                    remote.push_str(&format!(",user={}", q(user)));
                }
                if let Some(pw) = password {
                    env.push(("RCLONE_WEBDAV_PASS".into(), rclone_obscure(pw)?));
                }
                (format!("{remote}:"), "/".to_string())
            }
            Protocol::Ftp => {
                let user = self.user.as_deref().unwrap_or("anonymous");
                let mut remote = format!(":ftp,host={},port={},user={}", q(&self.host), self.port(), q(user));
                if self.tls() {
                    remote.push_str(",tls=true");
                }
                // rclone cannot log in without a password, even an empty one.
                let pw = match password {
                    Some(pw) => pw,
                    None if user == "anonymous" => "anonymous@",
                    None => anyhow::bail!("authentication failed: {user} needs a password"),
                };
                env.push(("RCLONE_FTP_PASS".into(), rclone_obscure(pw)?));
                (format!("{remote}:"), start)
            }
            Protocol::Rclone => {
                let name = self.host.trim_end_matches(':');
                if name.is_empty() || name.contains(['/', ' ']) {
                    anyhow::bail!("'{}' is not an rclone remote name", self.host);
                }
                (format!("{name}:"), start)
            }
            Protocol::Smb | Protocol::Nfs | Protocol::Sftp => {
                anyhow::bail!("{} connections do not use rclone", self.protocol.as_str())
            }
        };
        Ok(RcloneRemote { remote, env, home: PathBuf::from(home) })
    }

    pub fn mount_point(&self) -> PathBuf {
        self.mount_point.as_ref().map(|p| crate::util::expand_tilde(&p.to_string_lossy())).unwrap_or_else(|| {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("mnt").join(sanitize(&self.name))
        })
    }

    /// Where the share is currently mounted, if it is.
    pub fn mounted_at(&self) -> Option<PathBuf> {
        if self.protocol.in_app() {
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
            Protocol::Sftp | Protocol::S3 | Protocol::Webdav | Protocol::Ftp | Protocol::Rclone => return None,
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
            Protocol::Sftp | Protocol::S3 | Protocol::Webdav | Protocol::Ftp | Protocol::Rclone => false,
            Protocol::Smb => !(cfg!(target_os = "windows") || (cfg!(target_os = "linux") && which("gio"))),
            Protocol::Nfs => !cfg!(target_os = "windows"),
        }
    }
}

/// How rclone reaches a connection: see [`Connection::rclone_remote`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcloneRemote {
    /// Prefix for rclone paths, e.g. `:ftp,host='nas':` or `gdrive:`.
    pub remote: String,
    /// Environment variables holding secrets.
    pub env: Vec<(String, String)>,
    /// Where browsing starts.
    pub home: PathBuf,
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
/// Empty for rclone remotes, whose server strata does not know.
pub fn probe_ports(conn: &Connection) -> Vec<u16> {
    match (conn.protocol, conn.port) {
        (Protocol::Smb, None) => vec![445, 139],
        (Protocol::Rclone, _) => Vec::new(),
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
    if conn.protocol == Protocol::Rclone {
        // The server behind a configured remote is rclone's business.
        let rclone = rclone_step(&mut steps);
        let name = conn.host.trim_end_matches(':');
        let remotes = rclone.then(|| Command::new("rclone").arg("listremotes").output().ok()).flatten();
        let (status, detail) = match remotes {
            None => (StepStatus::Skipped, "rclone is not available".to_string()),
            Some(out) if String::from_utf8_lossy(&out.stdout).lines().any(|l| l.trim_end_matches(':') == name) => {
                (StepStatus::Ok, format!("{name}: is configured"))
            }
            Some(_) => (StepStatus::Fail, format!("no remote named {name}: (run rclone config)")),
        };
        steps.push(Step { name: "Remote", status, detail });
        return steps;
    }
    let resolved_conn;
    let conn = if conn.protocol == Protocol::Sftp {
        match crate::ssh::SshConfig::load().apply(conn) {
            Ok(applied) => {
                if applied != *conn {
                    let detail = format!(
                        "{} → {}@{}:{}",
                        conn.host,
                        applied.user.as_deref().unwrap_or("you"),
                        applied.host,
                        applied.port()
                    );
                    steps.push(Step { name: "SSH config", status: StepStatus::Ok, detail });
                }
                resolved_conn = applied;
                &resolved_conn
            }
            Err(e) => {
                steps.push(Step { name: "SSH config", status: StepStatus::Fail, detail: e.to_string() });
                return steps;
            }
        }
    } else {
        conn
    };
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
        Protocol::S3 | Protocol::Webdav | Protocol::Ftp | Protocol::Rclone => {
            rclone_step(&mut steps);
        }
        Protocol::Sftp => {
            let trusted = if reachable {
                let (status, detail, trusted) = host_key_step(conn);
                steps.push(Step { name: "Host key", status, detail });
                trusted
            } else {
                steps.push(Step {
                    name: "Host key",
                    status: StepStatus::Skipped,
                    detail: "server did not answer".into(),
                });
                false
            };
            steps.push(Step {
                name: "Credentials",
                status: if trusted { StepStatus::Ok } else { StepStatus::Skipped },
                detail: match (reachable, trusted) {
                    (true, true) => "checked when connecting (agent, key or password)".into(),
                    (true, false) => "not sent until the host key is trusted".into(),
                    (false, _) => "server did not answer".into(),
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

/// Looks the SFTP server's key up in `known_hosts`. The last value says
/// whether credentials would be sent: a new server counts, since
/// connecting asks the user first.
#[cfg(feature = "sftp")]
fn host_key_step(conn: &Connection) -> (StepStatus, String, bool) {
    use crate::ssh::{verify_host_key, HostKeyError};
    let (key, known) = match crate::vfs::probe_host_key(&conn.host, conn.port()) {
        Ok(found) => found,
        Err(e) => return (StepStatus::Fail, format!("{e:#}"), false),
    };
    let shown = format!("{} {}", key.key_type, key.fingerprint);
    match verify_host_key(known, key, None).map_err(|e| *e) {
        Ok(()) => (StepStatus::Ok, format!("known ({shown})"), true),
        Err(HostKeyError::Unknown(_)) => {
            (StepStatus::Warn, format!("new: {shown} · you will be asked to trust it"), true)
        }
        Err(e) => (StepStatus::Fail, e.to_string(), false),
    }
}

#[cfg(not(feature = "sftp"))]
fn host_key_step(_conn: &Connection) -> (StepStatus, String, bool) {
    (StepStatus::Skipped, "this build has no SFTP support".into(), false)
}

/// Checks that rclone is installed. Returns whether it is.
fn rclone_step(steps: &mut Vec<Step>) -> bool {
    let version = Command::new("rclone").arg("version").output().ok().filter(|o| o.status.success());
    let (status, detail) = match &version {
        Some(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            (StepStatus::Ok, text.lines().next().unwrap_or("installed").trim().to_string())
        }
        None => (StepStatus::Fail, "not installed (see https://rclone.org/install/)".to_string()),
    };
    steps.push(Step { name: "rclone", status, detail });
    version.is_some()
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
///
/// Results are cached: on WSL, `PATH` holds Windows folders where every
/// lookup is slow, and this runs on key presses. A miss is checked again
/// after a while, in case the program was installed meanwhile.
pub fn which(program: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<Mutex<HashMap<String, (bool, Instant)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let cached = cache.lock().ok().and_then(|c| c.get(program).copied());
    if let Some((found, at)) = cached {
        if found || at.elapsed() < Duration::from_secs(30) {
            return found;
        }
    }
    let found = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file() || candidate.with_extension("exe").is_file()
        })
    });
    if let Ok(mut c) = cache.lock() {
        c.insert(program.to_string(), (found, Instant::now()));
    }
    found
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
    fn parses_rclone_urls() {
        let s = Connection::from_url("", "s3://backups/daily/2024").unwrap();
        assert_eq!((s.protocol, s.name.as_str(), s.host.as_str()), (Protocol::S3, "backups", AWS_S3_HOST));
        assert_eq!(s.share, "/backups/daily/2024");
        assert_eq!(s.url(), "s3://backups/daily/2024");
        assert_eq!(s.port(), 443);

        let m = Connection::from_url("minio", "s3+http://key@minio.local:9000/bucket").unwrap();
        assert_eq!((m.host.as_str(), m.port(), m.tls()), ("minio.local", 9000, false));
        assert_eq!(m.url(), "s3+http://key@minio.local:9000/bucket");

        let d = Connection::from_url("cloud", "davs://me@cloud.example.com/remote.php/dav/files/me").unwrap();
        assert_eq!((d.protocol, d.port(), d.tls()), (Protocol::Webdav, 443, true));
        assert_eq!(d.url(), "davs://me@cloud.example.com/remote.php/dav/files/me");
        assert_eq!(Connection::from_url("", "dav://nas/dav").unwrap().port(), 80);

        let f = Connection::from_url("", "ftp://ftp.example.org/pub").unwrap();
        assert_eq!((f.protocol, f.port(), f.tls()), (Protocol::Ftp, 21, false));
        let fs = Connection::from_url("", "ftps://me@host/").unwrap();
        assert_eq!((fs.port(), fs.url()), (990, "ftps://me@host/".to_string()));

        let r = Connection::from_url("", "rclone://gdrive/Photos").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.share.as_str()), (Protocol::Rclone, "gdrive", "/Photos"));
        assert!(probe_ports(&r).is_empty());
        assert!(r.protocol.in_app() && r.mount_plan(None).is_none() && r.mounted_at().is_none());
        assert!(Connection::from_url("", "s3://").is_none());
    }

    #[test]
    fn builds_rclone_remotes_without_secrets_in_them() {
        let s = Connection::from_url("", "s3://backups/daily").unwrap();
        let r = s.rclone_remote(None).unwrap();
        assert_eq!(r.remote, ":s3,provider=AWS,env_auth=true:");
        let keyed = Connection::from_url("", "s3+https://AKIA@r2.example.com/b").unwrap();
        assert!(keyed.rclone_remote(None).unwrap_err().to_string().contains("authentication failed"));
        assert_eq!(r.home, PathBuf::from("/backups/daily"));

        let m = Connection::from_url("", "s3+http://AKIA@minio.local:9000/bucket").unwrap();
        let r = m.rclone_remote(Some("secret")).unwrap();
        assert_eq!(r.remote, ":s3,provider=Other,endpoint='http://minio.local:9000',access_key_id='AKIA':");
        assert_eq!(r.env, vec![("RCLONE_S3_SECRET_ACCESS_KEY".to_string(), "secret".to_string())]);

        let d = Connection::from_url("", "davs://me@cloud.example.com/remote.php/dav/files/me").unwrap();
        let r = d.rclone_remote(None).unwrap();
        assert_eq!(r.remote, ":webdav,url='https://cloud.example.com/remote.php/dav/files/me',user='me':");
        assert_eq!(r.home, PathBuf::from("/"));

        let f = Connection::from_url("", "ftps://me@[fd00::5]:2121/pub").unwrap();
        let err = f.rclone_remote(None).unwrap_err().to_string();
        assert!(err.contains("authentication failed"), "a named user is asked for a password: {err}");
        if crate::nas::which("rclone") {
            let r = f.rclone_remote(Some("pw")).unwrap();
            assert_eq!(r.remote, ":ftp,host='fd00::5',port=2121,user='me',tls=true:");
            assert_eq!(r.home, PathBuf::from("/pub"));
            assert_eq!(r.env[0].0, "RCLONE_FTP_PASS");
            assert_ne!(r.env[0].1, "pw", "rclone gets the password obscured");
            let anon = Connection::from_url("", "ftp://host/").unwrap().rclone_remote(None).unwrap();
            assert_eq!(anon.remote, ":ftp,host='host',port=21,user='anonymous':");
        }

        let g = Connection::from_url("", "rclone://gdrive").unwrap();
        assert_eq!(g.rclone_remote(None).unwrap().remote, "gdrive:");
        assert!(Connection::from_url("", "sftp://h/").unwrap().rclone_remote(None).is_err());
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
