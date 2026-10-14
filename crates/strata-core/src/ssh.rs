//! OpenSSH client files: the hosts in `~/.ssh/config` and the server keys
//! in `~/.ssh/known_hosts`, read the way `ssh` reads them so a host that
//! works on the command line works the same way here.

use std::collections::HashSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::nas::{Connection, Protocol};
use crate::util::{base64, base64_decode, expand_tilde, glob_match};

/// `~/.ssh`.
pub fn ssh_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".ssh")
}

/// How deep `Include` may nest, as in OpenSSH.
const MAX_INCLUDE_DEPTH: usize = 16;

/// One `Host` alias from `~/.ssh/config` with the settings that apply to it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshHost {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<PathBuf>,
    /// `ProxyJump …` or `ProxyCommand …`, which strata cannot follow yet.
    pub proxy: Option<String>,
}

impl SshHost {
    /// A connection to this alias. Only the alias is stored: the rest is
    /// read from `~/.ssh/config` again on every connect, so it stays the
    /// one place those settings live.
    pub fn to_connection(&self) -> Connection {
        Connection::new(&self.alias, Protocol::Sftp, &self.alias)
    }

    /// `user@host:port`, as far as the config says.
    pub fn address(&self) -> String {
        let user = self.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{user}{}{port}", self.host_name.as_deref().unwrap_or(&self.alias))
    }
}

/// A parsed `~/.ssh/config`. Only what an SFTP connection needs is used:
/// `HostName`, `User`, `Port`, `IdentityFile`, and `ProxyJump` or
/// `ProxyCommand` to say a host cannot be reached directly. `Match` blocks
/// are not evaluated and never apply.
#[derive(Debug, Clone, Default)]
pub struct SshConfig {
    blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
struct Block {
    patterns: Vec<String>,
    /// Patterns of the blocks holding the `Include`s this block came from;
    /// all of them must match too.
    within: Vec<Vec<String>>,
    /// Lowercased keyword and its value, in file order.
    options: Vec<(String, String)>,
}

impl Block {
    fn applies(&self, host: &str) -> bool {
        matches_patterns(&self.patterns, host) && self.within.iter().all(|p| matches_patterns(p, host))
    }
}

impl SshConfig {
    /// Reads `~/.ssh/config`; a missing or unreadable file is an empty config.
    pub fn load() -> Self {
        let dir = ssh_dir();
        Self::load_from(&dir.join("config"), &dir)
    }

    /// Reads `path`, resolving relative `Include`s against `ssh_dir`.
    pub fn load_from(path: &Path, ssh_dir: &Path) -> Self {
        Self::parse(&std::fs::read_to_string(path).unwrap_or_default(), ssh_dir)
    }

    pub fn parse(text: &str, ssh_dir: &Path) -> Self {
        // Options before the first `Host` apply to every host.
        let mut config =
            Self { blocks: vec![Block { patterns: vec!["*".into()], within: Vec::new(), options: Vec::new() }] };
        config.read(text, ssh_dir, &[], 0);
        config
    }

    fn read(&mut self, text: &str, ssh_dir: &Path, within: &[Vec<String>], depth: usize) {
        for line in text.lines() {
            let Some((keyword, args)) = split_line(line) else { continue };
            match keyword.as_str() {
                "host" => self.blocks.push(Block { patterns: args, within: within.to_vec(), options: Vec::new() }),
                "match" => {
                    let patterns = match args.first() {
                        Some(all) if all.eq_ignore_ascii_case("all") && args.len() == 1 => vec!["*".into()],
                        _ => Vec::new(),
                    };
                    self.blocks.push(Block { patterns, within: within.to_vec(), options: Vec::new() });
                }
                "include" if depth < MAX_INCLUDE_DEPTH => {
                    let Some(current) = self.blocks.last().cloned() else { continue };
                    let mut inner = current.within.clone();
                    inner.push(current.patterns.clone());
                    for pattern in &args {
                        for file in include_files(pattern, ssh_dir) {
                            if let Ok(text) = std::fs::read_to_string(&file) {
                                self.read(&text, ssh_dir, &inner, depth + 1);
                            }
                        }
                    }
                    // What follows the `Include` belongs to the block it sat in.
                    self.blocks.push(Block { options: Vec::new(), ..current });
                }
                _ if args.is_empty() => {}
                _ => {
                    // A proxy command is a whole command line; the rest take one value.
                    let value = if keyword == "proxycommand" { args.join(" ") } else { args[0].clone() };
                    if let Some(block) = self.blocks.last_mut() {
                        block.options.push((keyword, value));
                    }
                }
            }
        }
    }

    /// Every concrete alias (no wildcards or negation), in file order.
    pub fn hosts(&self) -> Vec<SshHost> {
        let mut aliases: Vec<&str> = Vec::new();
        for block in &self.blocks {
            for pattern in &block.patterns {
                let concrete = !pattern.contains(['*', '?', '!']);
                if concrete && block.applies(pattern) && !aliases.iter().any(|a| a.eq_ignore_ascii_case(pattern)) {
                    aliases.push(pattern);
                }
            }
        }
        aliases.into_iter().map(|alias| self.resolve(alias)).collect()
    }

    /// The settings `ssh <alias>` would use. The first value found wins.
    pub fn resolve(&self, alias: &str) -> SshHost {
        let mut host = SshHost { alias: alias.to_string(), ..SshHost::default() };
        let mut identity = None;
        let mut seen = HashSet::new();
        for block in self.blocks.iter().filter(|b| b.applies(alias)) {
            for (keyword, value) in &block.options {
                // ProxyJump and ProxyCommand: whichever comes first decides.
                let slot = if keyword == "proxycommand" { "proxyjump" } else { keyword.as_str() };
                if !seen.insert(slot) {
                    continue;
                }
                match keyword.as_str() {
                    "hostname" => host.host_name = Some(value.replace("%h", alias).replace("%%", "%")),
                    "user" => host.user = Some(value.clone()),
                    "port" => host.port = value.parse().ok(),
                    "identityfile" => identity = Some(value.clone()),
                    "proxyjump" | "proxycommand" if !value.eq_ignore_ascii_case("none") => {
                        let name = if keyword == "proxyjump" { "ProxyJump" } else { "ProxyCommand" };
                        host.proxy = Some(format!("{name} {value}"));
                    }
                    _ => {}
                }
            }
        }
        host.identity_file = identity.map(|file| {
            let home = dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned();
            let local = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
            let file = expand_tokens(
                &file,
                &[
                    ('d', &home),
                    ('h', host.host_name.as_deref().unwrap_or(alias)),
                    ('n', alias),
                    ('r', host.user.as_deref().unwrap_or(&local)),
                    ('u', &local),
                ],
            );
            expand_tilde(&file)
        });
        host
    }

    /// Fills in what `~/.ssh/config` says about an SFTP connection's host;
    /// settings in the connection itself win. Fails for a host reached
    /// through a jump host or proxy command, which strata cannot follow yet.
    pub fn apply(&self, conn: &Connection) -> anyhow::Result<Connection> {
        if conn.protocol != Protocol::Sftp {
            return Ok(conn.clone());
        }
        let host = self.resolve(&conn.host);
        if let Some(proxy) = &host.proxy {
            anyhow::bail!("{} is reached through {proxy} in ~/.ssh/config, which strata cannot follow yet", conn.host);
        }
        let mut out = conn.clone();
        if let Some(name) = host.host_name {
            out.host = name;
        }
        out.user = out.user.or(host.user);
        out.port = out.port.or(host.port);
        out.identity_file = out.identity_file.or(host.identity_file);
        Ok(out)
    }
}

/// Splits a config line into its lowercased keyword and arguments:
/// `Key value`, `Key=value` and `Key = "quoted value"` all work.
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    Some((keyword.to_ascii_lowercase(), split_args(rest)))
}

/// Whitespace-separated arguments with `"` or `'` quoting; an unquoted
/// `#` starts a comment. A backslash escapes only a quote, a space or
/// itself, so Windows paths survive.
fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (_, '\\') if matches!(chars.peek(), Some('"' | '\'' | ' ' | '\\')) => {
                current.push(chars.next().unwrap_or('\\'));
                in_arg = true;
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_arg = true;
            }
            (None, '#') if !in_arg => break,
            (None, c) if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    args
}

/// Whether `host` matches a pattern list: one positive match and no
/// negated (`!pattern`) match.
fn matches_patterns(patterns: &[String], host: &str) -> bool {
    let mut hit = false;
    for pattern in patterns {
        match pattern.strip_prefix('!') {
            Some(negated) if glob_match(negated, host) => return false,
            Some(_) => {}
            None => hit |= glob_match(pattern, host),
        }
    }
    hit
}

/// The files an `Include` names: `~` is expanded, relative paths are
/// under `~/.ssh` and wildcards in the file name are matched, sorted.
fn include_files(pattern: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = expand_tilde(pattern);
    let path = if path.is_absolute() { path } else { ssh_dir.join(path) };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return Vec::new() };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.file_name().is_some_and(|n| glob_match(&name, &n.to_string_lossy())))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Replaces `%x` tokens; `%%` is a literal `%`.
fn expand_tokens(text: &str, tokens: &[(char, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(t) => match tokens.iter().find(|(k, _)| *k == t) {
                Some((_, value)) => out.push_str(value),
                None => {
                    out.push('%');
                    out.push(t);
                }
            },
            None => out.push('%'),
        }
    }
    out
}

/// A server's host key, as offered during the SSH handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub host: String,
    pub port: u16,
    /// `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ssh-rsa`…
    pub key_type: String,
    /// `SHA256:…`, as `ssh` prints it.
    pub fingerprint: String,
    blob: Vec<u8>,
}

impl HostKey {
    /// `blob` is the key in SSH wire format, which starts with its type.
    pub fn new(host: &str, port: u16, blob: &[u8]) -> Self {
        let key_type = blob
            .get(..4)
            .map(|len| u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize)
            .and_then(|len| blob.get(4..4 + len))
            .and_then(|name| std::str::from_utf8(name).ok())
            .unwrap_or("unknown")
            .to_string();
        let fingerprint = format!("SHA256:{}", base64(&Sha256::digest(blob)).trim_end_matches('='));
        Self { host: host.to_string(), port, key_type, fingerprint, blob: blob.to_vec() }
    }

    /// How `known_hosts` names this server: `host`, or `[host]:port` off port 22.
    pub fn host_entry(&self) -> String {
        host_entry(&self.host, self.port)
    }

    /// The `known_hosts` line that trusts this key.
    pub fn known_hosts_line(&self) -> String {
        format!("{} {} {}", self.host_entry(), self.key_type, base64(&self.blob))
    }
}

fn host_entry(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

/// What `known_hosts` says about a server's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnownHost {
    Match,
    /// The server is not listed.
    NotFound,
    /// The server is listed, with other keys (of these types).
    Changed {
        known_types: Vec<String>,
    },
    /// The key is marked `@revoked`.
    Revoked,
}

/// Looks `key` up in the text of one or more `known_hosts` files. Hashed
/// names (`|1|salt|hash`), wildcards, negation and `[host]:port` entries
/// work as in OpenSSH; `@cert-authority` lines are ignored.
pub fn check_known_hosts(text: &str, key: &HostKey) -> KnownHost {
    let name = key.host_entry();
    let (mut matched, mut revoked) = (false, false);
    let mut known_types: Vec<String> = Vec::new();
    for entry in known_hosts_entries(text).filter(|e| e.names(&name)) {
        let same = base64_decode(entry.key).is_some_and(|k| k == key.blob);
        match entry.marker {
            Some("@revoked") => revoked |= same,
            Some(_) => {}
            None if same => matched = true,
            None => {
                if !known_types.iter().any(|t| t == entry.key_type) {
                    known_types.push(entry.key_type.to_string());
                }
            }
        }
    }
    match (revoked, matched) {
        (true, _) => KnownHost::Revoked,
        (false, true) => KnownHost::Match,
        (false, false) if known_types.is_empty() => KnownHost::NotFound,
        (false, false) => KnownHost::Changed { known_types },
    }
}

/// Key types `known_hosts` has for a server, so the handshake can ask for
/// one of those rather than a key that would look unknown.
pub fn known_key_types(text: &str, host: &str, port: u16) -> Vec<String> {
    let name = host_entry(host, port);
    let mut types: Vec<String> = Vec::new();
    for entry in known_hosts_entries(text).filter(|e| e.marker.is_none() && e.names(&name)) {
        if !types.iter().any(|t| t == entry.key_type) {
            types.push(entry.key_type.to_string());
        }
    }
    types
}

/// Decides whether to go on with a connection, before any credentials
/// are sent. A new server needs the user's word (`accepted` is the
/// fingerprint they approved); a changed or revoked key is refused.
pub fn verify_host_key(known: KnownHost, key: HostKey, accepted: Option<&str>) -> Result<(), Box<HostKeyError>> {
    let error = match known {
        KnownHost::Match => return Ok(()),
        KnownHost::NotFound if accepted == Some(key.fingerprint.as_str()) => return Ok(()),
        KnownHost::NotFound => HostKeyError::Unknown(key),
        KnownHost::Changed { known_types } => HostKeyError::Changed { key, known_types },
        KnownHost::Revoked => HostKeyError::Revoked(key),
    };
    Err(Box::new(error))
}

/// Why a server's key was not trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyError {
    /// Not in `known_hosts` yet: ask the user.
    Unknown(HostKey),
    /// Listed with a different key: possibly a man-in-the-middle.
    Changed {
        key: HostKey,
        known_types: Vec<String>,
    },
    Revoked(HostKey),
}

impl HostKeyError {
    pub fn key(&self) -> &HostKey {
        match self {
            Self::Unknown(key) | Self::Changed { key, .. } | Self::Revoked(key) => key,
        }
    }
}

impl fmt::Display for HostKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = self.key();
        let host = key.host_entry();
        match self {
            Self::Unknown(_) => {
                write!(f, "{host} is not in known_hosts yet; its {} key is {}", key.key_type, key.fingerprint)
            }
            Self::Changed { known_types, .. } if known_types.contains(&key.key_type) => write!(
                f,
                "HOST KEY CHANGED for {host}: the server's {} key is now {}, not the one in known_hosts. \
                 Someone may be intercepting the connection",
                key.key_type, key.fingerprint
            ),
            Self::Changed { known_types, .. } => write!(
                f,
                "HOST KEY NOT VERIFIED for {host}: known_hosts has its {} key, but the server offered \
                 a {} key ({}). Someone may be intercepting the connection",
                known_types.join("/"),
                key.key_type,
                key.fingerprint
            ),
            Self::Revoked(_) => {
                write!(f, "the {} key of {host} ({}) is marked @revoked in known_hosts", key.key_type, key.fingerprint)
            }
        }
    }
}

impl std::error::Error for HostKeyError {}

/// The user's and the system's `known_hosts` files, concatenated. Files
/// that cannot be read count as empty: the server is then unknown.
pub fn read_known_hosts() -> String {
    let dir = ssh_dir();
    let files = [
        dir.join("known_hosts"),
        dir.join("known_hosts2"),
        PathBuf::from("/etc/ssh/ssh_known_hosts"),
        PathBuf::from("/etc/ssh/ssh_known_hosts2"),
    ];
    files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).collect::<Vec<_>>().join("\n")
}

/// Appends `key` to `~/.ssh/known_hosts`, creating `~/.ssh` (0700) and
/// the file (0600) when missing. Returns the file written.
pub fn remember_host_key(key: &HostKey) -> std::io::Result<PathBuf> {
    let dir = ssh_dir();
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let path = dir.join("known_hosts");
    append_known_host(&path, key)?;
    Ok(path)
}

fn append_known_host(path: &Path, key: &HostKey) -> std::io::Result<()> {
    let existing = std::fs::read(path).unwrap_or_default();
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let newline = if existing.last().is_some_and(|&b| b != b'\n') { "\n" } else { "" };
    writeln!(file, "{newline}{}", key.known_hosts_line())
}

/// One key line of a `known_hosts` file.
struct KnownHostsEntry<'a> {
    marker: Option<&'a str>,
    hosts: &'a str,
    key_type: &'a str,
    key: &'a str,
}

impl KnownHostsEntry<'_> {
    /// Whether the entry names `name` (`host` or `[host]:port`).
    fn names(&self, name: &str) -> bool {
        if let Some(hashed) = self.hosts.strip_prefix("|1|") {
            let Some((salt, hash)) = hashed.split_once('|') else { return false };
            return match (base64_decode(salt), base64_decode(hash)) {
                (Some(salt), Some(hash)) => hmac_sha1(&salt, name.as_bytes()).as_slice() == hash.as_slice(),
                _ => false,
            };
        }
        let patterns: Vec<String> = self.hosts.split(',').map(str::to_string).collect();
        matches_patterns(&patterns, name)
    }
}

fn known_hosts_entries(text: &str) -> impl Iterator<Item = KnownHostsEntry<'_>> {
    text.lines().filter_map(|line| {
        let mut fields = line.split_whitespace().peekable();
        if fields.peek()?.starts_with('#') {
            return None;
        }
        let marker = fields.next_if(|f| f.starts_with('@'));
        Some(KnownHostsEntry { marker, hosts: fields.next()?, key_type: fields.next()?, key: fields.next()? })
    })
}

/// HMAC-SHA1 (RFC 2104), which hashes `known_hosts` names.
fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha1::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha1::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> SshConfig {
        SshConfig::parse(text, Path::new("/nonexistent"))
    }

    #[test]
    fn hosts_list_concrete_aliases_with_their_settings() {
        let c = config(
            "# my servers\n\
             Host nas backup\n\
             \x20   HostName 192.168.1.10\n\
             \x20   User admin\n\
             \x20   Port 2222\n\
             Host *.lan\n\
             \x20   User lan\n\
             Host *\n\
             \x20   User fallback\n\
             \x20   IdentityFile ~/.ssh/id_ed25519\n",
        );
        let hosts = c.hosts();
        assert_eq!(hosts.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(), ["nas", "backup"]);
        let nas = &hosts[0];
        assert_eq!(nas.host_name.as_deref(), Some("192.168.1.10"));
        assert_eq!(nas.user.as_deref(), Some("admin"), "the first value wins over Host *");
        assert_eq!(nas.port, Some(2222));
        assert_eq!(nas.identity_file, Some(expand_tilde("~/.ssh/id_ed25519")));
        assert_eq!(hosts[1].host_name.as_deref(), Some("192.168.1.10"), "aliases share their block");
        assert_eq!(nas.address(), "admin@192.168.1.10:2222");
        // Wildcard blocks still apply to names that are not aliases.
        assert_eq!(c.resolve("printer.lan").user.as_deref(), Some("lan"));
    }

    #[test]
    fn keywords_are_case_insensitive_and_take_equals_and_quotes() {
        let c = config(
            "HOST web\n\
             hostname=web.example.com\n\
             USER = deploy\n\
             IdentityFile \"/keys/my key\"\n\
             Port 22 # trailing comment\n",
        );
        let web = c.resolve("web");
        assert_eq!(web.host_name.as_deref(), Some("web.example.com"));
        assert_eq!(web.user.as_deref(), Some("deploy"));
        assert_eq!(web.identity_file, Some(PathBuf::from("/keys/my key")));
        assert_eq!(web.port, Some(22));
        assert_eq!(c.resolve("WEB").host_name.as_deref(), Some("web.example.com"), "host names ignore case");
    }

    #[test]
    fn wildcard_and_negated_patterns_are_not_listed() {
        let c = config("Host * !secret\nUser all\nHost secret\nHost db? *.prod\nHost !nope\n");
        let aliases: Vec<String> = c.hosts().into_iter().map(|h| h.alias).collect();
        assert_eq!(aliases, ["secret"]);
        assert_eq!(c.resolve("secret").user, None, "negated in Host * !secret");
        assert_eq!(c.resolve("other").user.as_deref(), Some("all"));
    }

    #[test]
    fn match_blocks_never_apply_except_match_all() {
        let c = config("Match host x exec true\nUser matched\nHost x\nPort 1\nMatch all\nUser everyone\n");
        let x = c.resolve("x");
        assert_eq!(x.user.as_deref(), Some("everyone"));
        assert_eq!(x.port, Some(1));
    }

    #[test]
    fn host_name_tokens_and_identity_tokens_expand() {
        let c = config("Host box\nHostName %h.example.com\nUser me\nIdentityFile %d/.ssh/%r@%h\n");
        let box_ = c.resolve("box");
        assert_eq!(box_.host_name.as_deref(), Some("box.example.com"));
        let home = dirs::home_dir().unwrap_or_default();
        assert_eq!(
            box_.identity_file,
            Some(PathBuf::from(format!("{}/.ssh/me@box.example.com", home.to_string_lossy())))
        );
        assert_eq!(expand_tokens("100%% %z", &[]), "100% %z");
    }

    #[test]
    fn proxies_are_recorded_and_none_turns_them_off() {
        let c = config(
            "Host inner\nProxyJump bastion\nHost cmd\nProxyCommand ssh -W %h:%p gw\nHost direct\nProxyJump none\n\
             Host *\nProxyJump everywhere\n",
        );
        assert_eq!(c.resolve("inner").proxy.as_deref(), Some("ProxyJump bastion"));
        assert_eq!(c.resolve("cmd").proxy.as_deref(), Some("ProxyCommand ssh -W %h:%p gw"));
        assert_eq!(c.resolve("direct").proxy, None, "the first value, none, wins");
        assert_eq!(c.resolve("other").proxy.as_deref(), Some("ProxyJump everywhere"));
    }

    #[test]
    fn includes_are_read_relative_to_ssh_dir_with_globs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("config.d")).unwrap();
        std::fs::write(dir.path().join("config.d/10-work"), "Host work\nHostName work.example.com\n").unwrap();
        std::fs::write(dir.path().join("config.d/20-home"), "Host home\nHostName 10.0.0.2\n").unwrap();
        std::fs::write(dir.path().join("extra"), "User included\n").unwrap();
        let text = "Include config.d/*\nHost later\nInclude extra\nPort 2200\n";
        let c = SshConfig::parse(text, dir.path());
        let aliases: Vec<String> = c.hosts().into_iter().map(|h| h.alias).collect();
        assert_eq!(aliases, ["work", "home", "later"]);
        assert_eq!(c.resolve("home").host_name.as_deref(), Some("10.0.0.2"));
        let later = c.resolve("later");
        assert_eq!(later.user.as_deref(), Some("included"), "options before a Host join the including block");
        assert_eq!(later.port, Some(2200), "the including block goes on after the Include");
        assert_eq!(c.resolve("work").user, None);
    }

    #[test]
    fn includes_inside_a_host_block_only_apply_to_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("inner"), "Host *\nUser inner\n").unwrap();
        let c = SshConfig::parse("Host a\nInclude inner\nHost b\n", dir.path());
        assert_eq!(c.resolve("a").user.as_deref(), Some("inner"));
        assert_eq!(c.resolve("b").user, None);
    }

    #[test]
    fn includes_that_loop_stop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("self"), "Include self\nHost looped\n").unwrap();
        let c = SshConfig::parse("Include self\n", dir.path());
        assert_eq!(c.hosts().len(), 1);
    }

    #[test]
    fn apply_fills_in_what_the_connection_leaves_out() {
        let c = config("Host nas\nHostName 10.0.0.5\nUser admin\nPort 2022\nIdentityFile /k\nHost hop\nProxyJump b\n");
        let mut conn = Connection::from_url("n", "sftp://me@nas/data").unwrap();
        let out = c.apply(&conn).unwrap();
        assert_eq!(out.host, "10.0.0.5");
        assert_eq!(out.user.as_deref(), Some("me"), "the connection's user wins");
        assert_eq!(out.port, Some(2022));
        assert_eq!(out.identity_file, Some(PathBuf::from("/k")));
        assert_eq!(out.name, "n");
        assert_eq!(out.share, "/data");
        conn.host = "hop".into();
        assert!(c.apply(&conn).unwrap_err().to_string().contains("ProxyJump b"));
        let smb = Connection::from_url("s", "smb://nas/share").unwrap();
        assert_eq!(c.apply(&smb).unwrap(), smb, "only SFTP reads ssh config");
        let alias = c.hosts()[0].to_connection();
        assert_eq!((alias.name.as_str(), alias.host.as_str(), alias.protocol), ("nas", "nas", Protocol::Sftp));
    }

    #[test]
    fn args_split_on_whitespace_and_quotes() {
        assert_eq!(split_args(r#"a "b c" 'd e' f\ g"#), ["a", "b c", "d e", "f g"]);
        assert_eq!(split_args(r"C:\Users\me\.ssh\id"), [r"C:\Users\me\.ssh\id"]);
        assert_eq!(split_args("x #comment"), ["x"]);
        assert_eq!(split_args("a#b"), ["a#b"]);
        assert_eq!(split_line("  # note"), None);
        assert_eq!(split_line(""), None);
    }

    /// An ed25519 key blob: type name then 32 key bytes.
    fn blob(byte: u8) -> Vec<u8> {
        let mut blob = Vec::new();
        blob.extend_from_slice(&11u32.to_be_bytes());
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&[byte; 32]);
        blob
    }

    #[test]
    fn host_keys_report_type_fingerprint_and_known_hosts_line() {
        let key = HostKey::new("NAS.local", 22, &blob(1));
        assert_eq!(key.key_type, "ssh-ed25519");
        let digest = Sha256::digest(blob(1));
        assert_eq!(key.fingerprint, format!("SHA256:{}", base64(&digest).trim_end_matches('=')));
        assert!(!key.fingerprint.ends_with('='));
        assert_eq!(key.known_hosts_line(), format!("nas.local ssh-ed25519 {}", base64(&blob(1))));
        assert_eq!(HostKey::new("nas", 2222, &blob(1)).host_entry(), "[nas]:2222");
        assert_eq!(HostKey::new("h", 22, b"\0\0").key_type, "unknown");
    }

    #[test]
    fn known_hosts_match_change_and_unknown() {
        let key = HostKey::new("nas", 22, &blob(1));
        let line = key.known_hosts_line();
        assert_eq!(check_known_hosts(&format!("# comment\n\n{line}\n"), &key), KnownHost::Match);
        assert_eq!(check_known_hosts("", &key), KnownHost::NotFound);
        let other = HostKey::new("nas", 22, &blob(2)).known_hosts_line();
        assert_eq!(check_known_hosts(&other, &key), KnownHost::Changed { known_types: vec!["ssh-ed25519".into()] });
        // Another key of a different type alongside the right one is fine.
        let rsa = format!("nas ssh-rsa {}", base64(b"rsa"));
        assert_eq!(check_known_hosts(&format!("{rsa}\n{line}"), &key), KnownHost::Match);
        assert_eq!(check_known_hosts(&rsa, &key), KnownHost::Changed { known_types: vec!["ssh-rsa".into()] });
        // A different server is not this one.
        let elsewhere = HostKey::new("other", 22, &blob(2)).known_hosts_line();
        assert_eq!(check_known_hosts(&elsewhere, &key), KnownHost::NotFound);
    }

    #[test]
    fn known_hosts_ports_patterns_and_markers() {
        let key = HostKey::new("nas", 2222, &blob(1));
        let b64 = base64(&blob(1));
        assert_eq!(check_known_hosts(&format!("[nas]:2222 ssh-ed25519 {b64}"), &key), KnownHost::Match);
        // OpenSSH keeps port 22 entries apart from other ports.
        assert_eq!(check_known_hosts(&format!("nas ssh-ed25519 {}", base64(&blob(2))), &key), KnownHost::NotFound);
        let on22 = HostKey::new("nas", 22, &blob(1));
        assert_eq!(check_known_hosts(&format!("other,NAS ssh-ed25519 {b64}"), &on22), KnownHost::Match);
        assert_eq!(check_known_hosts(&format!("*.lan,nas* ssh-ed25519 {b64}"), &on22), KnownHost::Match);
        assert_eq!(check_known_hosts(&format!("n*,!nas ssh-ed25519 {b64}"), &on22), KnownHost::NotFound);
        assert_eq!(
            check_known_hosts(&format!("@revoked * ssh-ed25519 {b64}\nnas ssh-ed25519 {b64}"), &on22),
            KnownHost::Revoked
        );
        assert_eq!(
            check_known_hosts(&format!("@cert-authority * ssh-ed25519 {}", base64(&blob(9))), &on22),
            KnownHost::NotFound
        );
    }

    #[test]
    fn hashed_known_hosts_names_match() {
        // `ssh-keygen -H` writes |1|base64(salt)|base64(HMAC-SHA1(salt, name)).
        let salt = [7u8; 20];
        let hashed = |name: &str| format!("|1|{}|{}", base64(&salt), base64(&hmac_sha1(&salt, name.as_bytes())));
        let key = HostKey::new("nas", 22, &blob(1));
        let b64 = base64(&blob(1));
        assert_eq!(check_known_hosts(&format!("{} ssh-ed25519 {b64}", hashed("nas")), &key), KnownHost::Match);
        assert_eq!(check_known_hosts(&format!("{} ssh-ed25519 {b64}", hashed("other")), &key), KnownHost::NotFound);
        let off22 = HostKey::new("nas", 2222, &blob(1));
        assert_eq!(check_known_hosts(&format!("{} ssh-ed25519 {b64}", hashed("[nas]:2222")), &off22), KnownHost::Match);
    }

    #[test]
    fn real_ssh_keygen_output_agrees() {
        // From `ssh-keygen -t ed25519`, `ssh-keygen -H` and `ssh-keygen -l`.
        let b64 = "AAAAC3NzaC1lZDI1NTE5AAAAIIqEiDZ8gVZC50DxdYhU0pQErZiETYs52V7iJailGHE+";
        let key = HostKey::new("nas.local", 2222, &base64_decode(b64).unwrap());
        assert_eq!(key.fingerprint, "SHA256:FPPeVGAYIx4dgaUHp65DWTmJtgeGPGDWcZAKQlY6EHY");
        let hashed = format!("|1|JNfVShrQdJn9dyGfgsuekmeRrTo=|8DTZE149CS3XffjuDtlYcDIQaxo= ssh-ed25519 {b64}");
        assert_eq!(check_known_hosts(&hashed, &key), KnownHost::Match);
        assert_eq!(key.known_hosts_line(), format!("[nas.local]:2222 ssh-ed25519 {b64}"));
    }

    #[test]
    fn hmac_sha1_matches_rfc2202() {
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(&hmac_sha1(&[0x0b; 20], b"Hi There")), "b617318655057264e28bc0b6fb378c8ef146be00");
        assert_eq!(
            hex(&hmac_sha1(b"Jefe", b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        assert_eq!(
            hex(&hmac_sha1(&[0xaa; 80], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn known_key_types_ignore_markers_and_other_hosts() {
        let text = format!(
            "nas ssh-ed25519 {a}\nnas ssh-rsa {a}\nnas ssh-ed25519 {a}\n@revoked nas ecdsa-sha2-nistp256 {a}\nx ssh-dss {a}",
            a = base64(b"k")
        );
        assert_eq!(known_key_types(&text, "nas", 22), ["ssh-ed25519", "ssh-rsa"]);
        assert!(known_key_types(&text, "nas", 2222).is_empty());
    }

    #[test]
    fn verify_asks_for_new_hosts_and_refuses_changed_ones() {
        let key = HostKey::new("nas", 22, &blob(1));
        let fp = key.fingerprint.clone();
        assert_eq!(verify_host_key(KnownHost::Match, key.clone(), None), Ok(()));
        assert_eq!(
            verify_host_key(KnownHost::NotFound, key.clone(), None),
            Err(Box::new(HostKeyError::Unknown(key.clone())))
        );
        assert_eq!(verify_host_key(KnownHost::NotFound, key.clone(), Some(&fp)), Ok(()));
        assert!(
            verify_host_key(KnownHost::NotFound, key.clone(), Some("SHA256:other")).is_err(),
            "approval covers only the key the user saw"
        );
        let changed = KnownHost::Changed { known_types: vec!["ssh-ed25519".into()] };
        let err = verify_host_key(changed.clone(), key.clone(), Some(&fp)).unwrap_err();
        assert!(matches!(*err, HostKeyError::Changed { .. }), "no approval gets past a changed key");
        assert!(err.to_string().contains("HOST KEY CHANGED"));
        let other_type = KnownHost::Changed { known_types: vec!["ssh-rsa".into()] };
        assert!(verify_host_key(other_type, key.clone(), None).unwrap_err().to_string().contains("NOT VERIFIED"));
        assert_eq!(
            verify_host_key(KnownHost::Revoked, key.clone(), Some(&fp)),
            Err(Box::new(HostKeyError::Revoked(key)))
        );
    }

    #[test]
    fn remembered_keys_are_appended_on_their_own_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, "old ssh-rsa AAAA").unwrap();
        let key = HostKey::new("nas", 2222, &blob(1));
        append_known_host(&path, &key).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, format!("old ssh-rsa AAAA\n{}\n", key.known_hosts_line()));
        assert_eq!(check_known_hosts(&text, &key), KnownHost::Match);

        let fresh = dir.path().join("new_known_hosts");
        append_known_host(&fresh, &key).unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), format!("{}\n", key.known_hosts_line()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
