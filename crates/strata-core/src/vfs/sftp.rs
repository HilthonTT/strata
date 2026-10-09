use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use ssh2::{FileStat, Session, Sftp};

use super::Vfs;
use crate::{Entry, EntryKind};

/// How to authenticate against an SFTP server.
#[derive(Debug, Clone, Default)]
pub enum SftpAuth {
    /// Try the SSH agent, then the default key files in `~/.ssh`.
    #[default]
    Auto,
    Key(PathBuf),
    Password(String),
}

/// A remote filesystem over SFTP (works with virtually every NAS).
pub struct SftpVfs {
    label: String,
    host: String,
    port: u16,
    user: String,
    home: PathBuf,
    // ssh2 handles are not safe for concurrent use; serialise all calls.
    sftp: Mutex<Sftp>,
    _session: Session,
}

impl std::fmt::Debug for SftpVfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpVfs")
            .field("label", &self.label)
            .finish()
    }
}

impl SftpVfs {
    pub fn connect(host: &str, port: u16, user: &str, auth: &SftpAuth) -> Result<Self> {
        let addr = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("cannot resolve {host}"))?
            .next()
            .with_context(|| format!("no address for {host}"))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(8))
            .with_context(|| format!("cannot reach {host}:{port}"))?;
        let mut session = Session::new()?;
        session.set_tcp_stream(tcp);
        session.set_timeout(15_000);
        session.handshake().context("SSH handshake failed")?;

        match auth {
            SftpAuth::Password(pw) => session.userauth_password(user, pw)?,
            SftpAuth::Key(key) => session.userauth_pubkey_file(user, None, key, None)?,
            SftpAuth::Auto => {
                if session.userauth_agent(user).is_err() {
                    let ssh_dir = dirs::home_dir().unwrap_or_default().join(".ssh");
                    for key in ["id_ed25519", "id_ecdsa", "id_rsa"] {
                        let key = ssh_dir.join(key);
                        if key.exists()
                            && session.userauth_pubkey_file(user, None, &key, None).is_ok()
                        {
                            break;
                        }
                    }
                }
            }
        }
        if !session.authenticated() {
            bail!("authentication failed for {user}@{host}");
        }

        let sftp = session
            .sftp()
            .context("server refused the SFTP subsystem")?;
        let home = sftp
            .realpath(Path::new("."))
            .unwrap_or_else(|_| PathBuf::from("/"));
        Ok(Self {
            label: format!("{user}@{host}"),
            host: host.to_string(),
            port,
            user: user.to_string(),
            home,
            sftp: Mutex::new(sftp),
            _session: session,
        })
    }

    fn entry_from(path: PathBuf, stat: &FileStat, to_dir: bool) -> Entry {
        let kind = match stat.file_type() {
            ft if ft.is_symlink() => EntryKind::Symlink { to_dir },
            ft if ft.is_dir() => EntryKind::Dir,
            ft if ft.is_file() => EntryKind::File,
            _ => EntryKind::Other,
        };
        Entry {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".into()),
            path,
            kind,
            size: if kind == EntryKind::Dir {
                0
            } else {
                stat.size.unwrap_or(0)
            },
            modified: stat.mtime.map(|t| UNIX_EPOCH + Duration::from_secs(t)),
            mode: stat.perm.map(|p| p & 0o7777),
        }
    }

    fn sftp(&self) -> std::sync::MutexGuard<'_, Sftp> {
        self.sftp.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Vfs for SftpVfs {
    fn scheme(&self) -> &'static str {
        "sftp"
    }

    fn label(&self) -> String {
        format!("sftp:{}", self.label)
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<Entry>> {
        let sftp = self.sftp();
        let items = sftp
            .readdir(path)
            .with_context(|| format!("read {}", path.display()))?;
        Ok(items
            .into_iter()
            .filter(|(p, _)| {
                !matches!(
                    p.file_name().and_then(|n| n.to_str()),
                    Some(".") | Some("..")
                )
            })
            .map(|(p, st)| {
                let to_dir = st.file_type().is_symlink()
                    && sftp.stat(&p).map(|s| s.is_dir()).unwrap_or(false);
                Self::entry_from(p, &st, to_dir)
            })
            .collect())
    }

    fn stat(&self, path: &Path) -> Result<Entry> {
        let sftp = self.sftp();
        let st = sftp
            .lstat(path)
            .with_context(|| format!("stat {}", path.display()))?;
        let to_dir =
            st.file_type().is_symlink() && sftp.stat(path).map(|s| s.is_dir()).unwrap_or(false);
        Ok(Self::entry_from(path.to_path_buf(), &st, to_dir))
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        Ok(self.sftp().mkdir(path, 0o755)?)
    }

    fn create_file(&self, path: &Path) -> Result<()> {
        self.sftp().create(path)?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        Ok(self.sftp().unlink(path)?)
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        Ok(self.sftp().rmdir(path)?)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        Ok(self.sftp().rename(from, to, None)?)
    }

    fn reader(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(self.sftp().open(path)?))
    }

    fn writer(&self, path: &Path) -> Result<Box<dyn Write + Send>> {
        Ok(Box::new(self.sftp().create(path)?))
    }

    fn shell_command(&self, path: &Path) -> Option<Vec<String>> {
        let dir = crate::util::shell_quote(&crate::util::posix(path));
        Some(vec![
            "ssh".into(),
            "-t".into(),
            "-p".into(),
            self.port.to_string(),
            format!("{}@{}", self.user, self.host),
            format!("cd {dir} && exec \"${{SHELL:-sh}}\" -l"),
        ])
    }
}
