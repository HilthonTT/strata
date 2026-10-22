//! Plugins installed from git repositories (`strata plugin add|update|…`).
//!
//! Each one is cloned, checked out at a commit and copied without its
//! history into `<config>/plugins/<name>/`, where the host loads its
//! `init.lua` like any hand-made plugin. `<config>/plugins.toml` records
//! where each came from and the commit, so `strata plugin install` on
//! another machine brings back exactly the same plugins.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::host::OFFICIAL_PLUGINS;

const MANIFEST_HEADER: &str = "\
# Plugins installed with `strata plugin`. Keep this file with your dotfiles:
# `strata plugin install` installs the same plugins, at the same commits.
";

/// Written into each installed plugin: the commit its files come from.
const REV_FILE: &str = ".strata-rev";

/// One plugin in `plugins.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    /// `owner/repo` (GitHub), a git URL or a local repository.
    pub source: String,
    /// Folder inside the repository holding the plugin, for repositories
    /// with several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Branch, tag or commit to follow; the default branch when unset.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The commit installed.
    pub rev: String,
}

impl Package {
    pub fn short_rev(&self) -> &str {
        &self.rev[..self.rev.len().min(7)]
    }

    /// `source`, plus `path` when there is one.
    pub fn origin(&self) -> String {
        match &self.path {
            Some(p) => format!("{} ({p})", self.source),
            None => self.source.clone(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default, rename = "plugin")]
    plugins: Vec<Package>,
}

/// What `strata plugin add` was asked for.
#[derive(Debug, Clone, Default)]
pub struct AddRequest {
    pub source: String,
    pub name: Option<String>,
    pub path: Option<String>,
    pub reference: Option<String>,
    /// Replace a plugin of the same name that was not installed this way.
    pub force: bool,
}

/// How an installed plugin compares with `plugins.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Installed,
    /// In `plugins.toml` but its folder is gone.
    Missing,
    /// Its files come from another commit than the one recorded.
    Stale,
}

/// One row of `strata plugin list`.
#[derive(Debug, Clone)]
pub enum Listed {
    Managed(Package, State),
    /// A plugin in the folder that `strata plugin` does not manage, and
    /// the file or folder it loads from.
    Local(String, PathBuf),
}

/// What happened to one plugin during `update` or `install`.
#[derive(Debug)]
pub enum Outcome {
    Unchanged(Package),
    /// Put back at the recorded commit by `install`.
    Restored(Package),
    Changed {
        from: String,
        to: Package,
    },
    Failed(String, anyhow::Error),
}

/// Plugins under one config directory.
pub struct Packages {
    plugin_dir: PathBuf,
    manifest: PathBuf,
}

impl Packages {
    pub fn new(config_dir: &Path) -> Self {
        Self { plugin_dir: config_dir.join("plugins"), manifest: config_dir.join("plugins.toml") }
    }

    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest
    }

    /// Clones `req.source` and installs it, replacing an earlier install of
    /// the same name.
    pub fn add(&self, req: &AddRequest) -> Result<Package> {
        let source = normalize_source(&req.source)?;
        let path = req.path.as_deref().map(|p| p.trim_matches('/').to_string()).filter(|p| !p.is_empty());
        let name = match &req.name {
            Some(n) => n.clone(),
            None => default_name(&source, path.as_deref()),
        };
        check_name(&name)?;
        let mut manifest = self.load()?;
        let managed = manifest.plugins.iter().any(|p| p.name == name);
        if !managed && !req.force && self.existing(&name).is_some() {
            bail!(
                "a plugin named '{name}' is already in {}; pick another name with --name, or --force to replace it",
                self.plugin_dir.display()
            );
        }
        let mut package = Package { name, source, path, reference: req.reference.clone(), rev: String::new() };
        package.rev = self.fetch(&package, package.reference.as_deref())?;
        manifest.plugins.retain(|p| p.name != package.name);
        manifest.plugins.push(package.clone());
        self.save(&manifest)?;
        Ok(package)
    }

    /// Deletes a managed plugin and its `plugins.toml` entry.
    pub fn remove(&self, name: &str) -> Result<Package> {
        let mut manifest = self.load()?;
        let Some(i) = manifest.plugins.iter().position(|p| p.name == name) else {
            match self.existing(name) {
                Some(p) => bail!("'{name}' was not installed with `strata plugin`; delete {} yourself", p.display()),
                None => bail!("no plugin named '{name}'"),
            }
        };
        let package = manifest.plugins.remove(i);
        let dir = self.plugin_dir.join(name);
        if dir.exists() {
            remove_tree(&dir).with_context(|| format!("deleting {}", dir.display()))?;
        }
        self.save(&manifest)?;
        Ok(package)
    }

    /// Moves the named plugins (all when `names` is empty) to the newest
    /// commit of the ref they follow.
    pub fn update(&self, names: &[String]) -> Result<Vec<Outcome>> {
        self.each(names, |pkg| Ok((self.fetch(pkg, pkg.reference.as_deref())?, false)))
    }

    /// Makes the plugin folder match `plugins.toml`: installs what is
    /// missing or at another commit, at the recorded commit.
    pub fn install(&self) -> Result<Vec<Outcome>> {
        self.each(&[], |pkg| {
            if self.state(pkg) == State::Installed {
                Ok((pkg.rev.clone(), false))
            } else {
                Ok((self.fetch(pkg, Some(&pkg.rev))?, true))
            }
        })
    }

    /// Managed plugins with their state, then the hand-made ones.
    pub fn list(&self) -> Result<Vec<Listed>> {
        let manifest = self.load()?;
        let mut out: Vec<Listed> = manifest.plugins.iter().map(|p| Listed::Managed(p.clone(), self.state(p))).collect();
        let mut local: Vec<(String, PathBuf)> = std::fs::read_dir(&self.plugin_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter_map(|p| {
                let name = if p.is_dir() {
                    p.join("init.lua").exists().then(|| p.file_name())??
                } else if p.extension().is_some_and(|e| e == "lua") {
                    p.file_stem()?
                } else {
                    return None;
                };
                Some((name.to_string_lossy().into_owned(), p.clone()))
            })
            .filter(|(n, _)| !n.starts_with('.') && !manifest.plugins.iter().any(|p| &p.name == n))
            .collect();
        local.sort();
        out.extend(local.into_iter().map(|(name, path)| Listed::Local(name, path)));
        Ok(out)
    }

    /// Runs `fetch` for the selected plugins, recording new commits.
    /// `fetch` returns the commit and whether it put a plugin back at the
    /// recorded one. One plugin failing does not stop the others.
    fn each(&self, names: &[String], fetch: impl Fn(&Package) -> Result<(String, bool)>) -> Result<Vec<Outcome>> {
        let mut manifest = self.load()?;
        if let Some(unknown) = names.iter().find(|n| !manifest.plugins.iter().any(|p| &p.name == *n)) {
            bail!("no plugin named '{unknown}' in {}", self.manifest.display());
        }
        let mut outcomes = Vec::new();
        for pkg in manifest.plugins.iter_mut().filter(|p| names.is_empty() || names.contains(&p.name)) {
            outcomes.push(match fetch(pkg) {
                Ok((rev, true)) if rev == pkg.rev => Outcome::Restored(pkg.clone()),
                Ok((rev, _)) if rev == pkg.rev => Outcome::Unchanged(pkg.clone()),
                Ok((rev, _)) => {
                    let from = std::mem::replace(&mut pkg.rev, rev);
                    Outcome::Changed { from, to: pkg.clone() }
                }
                Err(e) => Outcome::Failed(pkg.name.clone(), e),
            });
        }
        if outcomes.iter().any(|o| matches!(o, Outcome::Changed { .. })) {
            self.save(&manifest)?;
        }
        Ok(outcomes)
    }

    fn state(&self, pkg: &Package) -> State {
        let dir = self.plugin_dir.join(&pkg.name);
        if !dir.join("init.lua").exists() {
            return State::Missing;
        }
        match std::fs::read_to_string(dir.join(REV_FILE)) {
            Ok(rev) if rev.trim() == pkg.rev => State::Installed,
            _ => State::Stale,
        }
    }

    /// The file or folder a plugin called `name` loads from, if any.
    fn existing(&self, name: &str) -> Option<PathBuf> {
        [self.plugin_dir.join(name), self.plugin_dir.join(format!("{name}.lua"))].into_iter().find(|p| p.exists())
    }

    /// Clones the package at `reference` (the default branch when `None`)
    /// and puts its files in place. Returns the commit installed.
    fn fetch(&self, pkg: &Package, reference: Option<&str>) -> Result<String> {
        std::fs::create_dir_all(&self.plugin_dir).with_context(|| format!("creating {}", self.plugin_dir.display()))?;
        // Dot-named folders are never loaded as plugins, so a half-done
        // install is invisible to a strata starting meanwhile.
        let clone = self.plugin_dir.join(format!(".{}.clone", pkg.name));
        let staged = self.plugin_dir.join(format!(".{}.new", pkg.name));
        for dir in [&clone, &staged] {
            if dir.exists() {
                remove_tree(dir)?;
            }
        }
        let result = (|| {
            let url = clone_url(&pkg.source);
            git(None, &["clone", "--quiet", "--no-checkout", &url, &clone.to_string_lossy()])
                .with_context(|| format!("cloning {}", pkg.source))?;
            let rev = resolve(&clone, reference)?;
            git(Some(&clone), &["checkout", "--quiet", "--detach", &rev])?;
            let root = match &pkg.path {
                Some(p) => clone.join(p),
                None => clone.clone(),
            };
            if !root.join("init.lua").is_file() {
                let at = pkg.path.as_deref().map(|p| format!(" in {p}/")).unwrap_or_default();
                bail!("{} has no init.lua{at}", pkg.source);
            }
            std::fs::rename(&root, &staged).with_context(|| format!("moving {}", root.display()))?;
            let history = staged.join(".git");
            if history.exists() {
                remove_tree(&history)?;
            }
            std::fs::write(staged.join(REV_FILE), format!("{rev}\n"))?;
            let target = self.plugin_dir.join(&pkg.name);
            for old in [target.clone(), self.plugin_dir.join(format!("{}.lua", pkg.name))] {
                if old.is_dir() {
                    remove_tree(&old)?;
                } else if old.exists() {
                    std::fs::remove_file(&old)?;
                }
            }
            std::fs::rename(&staged, &target).with_context(|| format!("moving into {}", target.display()))?;
            Ok(rev)
        })();
        for dir in [&clone, &staged] {
            if dir.exists() {
                let _ = remove_tree(dir);
            }
        }
        result
    }

    fn load(&self) -> Result<Manifest> {
        match std::fs::read_to_string(&self.manifest) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", self.manifest.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.manifest.display())),
        }
    }

    fn save(&self, manifest: &Manifest) -> Result<()> {
        let mut text = MANIFEST_HEADER.to_string();
        if !manifest.plugins.is_empty() {
            text.push('\n');
            text.push_str(&toml::to_string(manifest)?);
        }
        let tmp = self.manifest.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.manifest).with_context(|| format!("writing {}", self.manifest.display()))
    }
}

/// `owner/repo` stays as it is; local paths become absolute so the
/// manifest works from any directory. Anything else is taken as a URL.
fn normalize_source(source: &str) -> Result<String> {
    let source = source.trim();
    if source.is_empty() {
        bail!("no source given");
    }
    if is_shorthand(source) {
        return Ok(source.trim_end_matches(".git").to_string());
    }
    if source.contains("://") || source.starts_with("git@") {
        return Ok(source.to_string());
    }
    let path = match source.strip_prefix("~/") {
        Some(rest) => dirs_home().join(rest),
        None => PathBuf::from(source),
    };
    let abs = std::fs::canonicalize(&path).with_context(|| format!("{source}: no such repository"))?;
    // git does not take Windows' `\\?\` verbatim paths.
    let abs = abs.to_string_lossy();
    Ok(abs.strip_prefix(r"\\?\").unwrap_or(&abs).to_string())
}

/// `owner/repo`: one slash, no scheme, not a relative path.
fn is_shorthand(s: &str) -> bool {
    let mut parts = s.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty() && p != "." && p != ".." && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none() && !s.starts_with('.')
}

fn clone_url(source: &str) -> String {
    if is_shorthand(source) {
        format!("https://github.com/{source}.git")
    } else {
        source.to_string()
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

/// The folder in `path`, else the repository name without `strata-`,
/// `.strata` and `.git`: `user/strata-foo.git` → `foo`.
fn default_name(source: &str, path: Option<&str>) -> String {
    let last = |s: &str| s.trim_end_matches(['/', '\\']).rsplit(['/', '\\', ':']).next().unwrap_or("").to_string();
    if let Some(p) = path {
        return last(p);
    }
    let repo = last(source);
    let repo = repo.strip_suffix(".git").unwrap_or(&repo);
    let repo = repo.strip_suffix(".strata").or_else(|| repo.strip_suffix("-strata")).unwrap_or(repo);
    repo.strip_prefix("strata-").or_else(|| repo.strip_prefix("strata.")).unwrap_or(repo).to_string()
}

/// Names become folder names and `require` paths.
fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with(['.', '-'])
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
    {
        bail!("'{name}' is not a valid plugin name (letters, digits, - and _); choose one with --name");
    }
    if OFFICIAL_PLUGINS.iter().any(|(n, _)| *n == name) {
        bail!("'{name}' is the name of an official plugin; choose another with --name");
    }
    Ok(())
}

/// The commit `reference` names in the clone: a tag, a commit, or a branch
/// (which only exists as `origin/<branch>` until checked out).
fn resolve(repo: &Path, reference: Option<&str>) -> Result<String> {
    let Some(r) = reference else {
        return git(Some(repo), &["rev-parse", "HEAD"]);
    };
    for candidate in [format!("{r}^{{commit}}"), format!("origin/{r}^{{commit}}")] {
        if let Ok(rev) = git(Some(repo), &["rev-parse", "--verify", "--quiet", &candidate]) {
            return Ok(rev);
        }
    }
    bail!("no branch, tag or commit '{r}' in the repository")
}

/// Runs git and returns its trimmed output; its stderr becomes the error.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    // Never stop to ask for a password: a private repository fails instead.
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0");
    let out = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow::anyhow!("git is needed to install plugins; install it first"),
        _ => anyhow::Error::from(e).context("running git"),
    })?;
    if !out.status.success() {
        // The last line says what went wrong; the ones before are progress.
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("git failed").trim();
        bail!("{}", err.trim_start_matches("fatal: ").trim_start_matches("error: "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `remove_dir_all` that also deletes read-only files (git's objects are
/// read-only on Windows).
fn remove_tree(dir: &Path) -> Result<()> {
    if std::fs::remove_dir_all(dir).is_ok() {
        return Ok(());
    }
    for entry in walk(dir) {
        if let Ok(meta) = std::fs::symlink_metadata(&entry) {
            let mut perms = meta.permissions();
            if perms.readonly() && !meta.file_type().is_symlink() {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(&entry, perms);
            }
        }
    }
    std::fs::remove_dir_all(dir).with_context(|| format!("deleting {}", dir.display()))
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(path.clone());
            }
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repository called `strata-demo` with `files` committed on its
    /// default branch.
    struct Repo(tempfile::TempDir);

    impl Repo {
        fn path(&self) -> PathBuf {
            self.0.path().join("strata-demo")
        }
    }

    fn repo(files: &[(&str, &str)]) -> Repo {
        let repo = Repo(tempfile::tempdir().unwrap());
        let dir = repo.path();
        std::fs::create_dir(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]);
        run(&dir, &["config", "user.email", "t@example.com"]);
        run(&dir, &["config", "user.name", "t"]);
        run(&dir, &["config", "core.autocrlf", "false"]);
        commit(&dir, files);
        repo
    }

    fn commit(dir: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "-q", "-m", "change"]);
    }

    fn head(dir: &Path) -> String {
        git(Some(dir), &["rev-parse", "HEAD"]).unwrap()
    }

    fn add(pkgs: &Packages, source: &Path) -> Result<Package> {
        pkgs.add(&AddRequest { source: source.to_string_lossy().into_owned(), ..Default::default() })
    }

    #[test]
    fn sources_and_names() {
        assert!(is_shorthand("someone/strata-foo"));
        assert!(!is_shorthand("./foo/bar"));
        assert!(!is_shorthand("https://example.com/a/b"));
        assert!(!is_shorthand("a/b/c"));
        assert_eq!(clone_url("a/b"), "https://github.com/a/b.git");
        assert_eq!(normalize_source("a/b.git").unwrap(), "a/b");
        assert_eq!(default_name("a/strata-foo", None), "foo");
        assert_eq!(default_name("https://example.com/x/bar.strata.git", None), "bar");
        assert_eq!(default_name("git@github.com:x/baz-strata.git", None), "baz");
        assert_eq!(default_name("a/plugins", Some("plugins/qux/")), "qux");
        assert!(check_name("git").is_err());
        assert!(check_name("../evil").is_err());
        assert!(check_name("good_name-2").is_ok());
    }

    #[test]
    fn add_update_remove() {
        let src = repo(&[("init.lua", "-- v1"), ("README.md", "hi")]);
        let config = tempfile::tempdir().unwrap();
        let pkgs = Packages::new(config.path());

        let pkg = add(&pkgs, &src.path()).unwrap();
        let dir = pkgs.plugin_dir().join(&pkg.name);
        assert_eq!(std::fs::read_to_string(dir.join("init.lua")).unwrap(), "-- v1");
        assert!(!dir.join(".git").exists());
        assert_eq!(pkg.rev, head(&src.path()));
        assert!(matches!(&pkgs.list().unwrap()[..], [Listed::Managed(p, State::Installed)] if p.name == pkg.name));

        let outcomes = pkgs.update(&[]).unwrap();
        assert!(matches!(&outcomes[..], [Outcome::Unchanged(_)]));

        commit(&src.path(), &[("init.lua", "-- v2")]);
        let outcomes = pkgs.update(std::slice::from_ref(&pkg.name)).unwrap();
        assert!(
            matches!(&outcomes[..], [Outcome::Changed { from, to }] if *from == pkg.rev && to.rev == head(&src.path()))
        );
        assert_eq!(std::fs::read_to_string(dir.join("init.lua")).unwrap(), "-- v2");
        let text = std::fs::read_to_string(pkgs.manifest_path()).unwrap();
        assert!(text.starts_with("# Plugins installed") && text.contains(&head(&src.path())), "{text}");

        pkgs.remove(&pkg.name).unwrap();
        assert!(!dir.exists());
        assert!(pkgs.list().unwrap().is_empty());
        assert!(pkgs.remove(&pkg.name).is_err());
    }

    #[test]
    fn refs_paths_and_install() {
        let src = repo(&[("plugins/one/init.lua", "-- old"), ("plugins/two/init.lua", "-- two")]);
        run(&src.path(), &["tag", "v1"]);
        commit(&src.path(), &[("plugins/one/init.lua", "-- new")]);
        let config = tempfile::tempdir().unwrap();
        let pkgs = Packages::new(config.path());

        let pkg = pkgs
            .add(&AddRequest {
                source: src.path().to_string_lossy().into_owned(),
                path: Some("plugins/one".into()),
                reference: Some("v1".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pkg.name, "one");
        let init = pkgs.plugin_dir().join("one/init.lua");
        assert_eq!(std::fs::read_to_string(&init).unwrap(), "-- old");
        assert!(!pkgs.plugin_dir().join("one/plugins").exists());
        // A tag does not move, so there is nothing to update.
        assert!(matches!(&pkgs.update(&[]).unwrap()[..], [Outcome::Unchanged(_)]));

        // On another machine: only plugins.toml came along.
        std::fs::remove_dir_all(pkgs.plugin_dir()).unwrap();
        assert!(matches!(&pkgs.list().unwrap()[..], [Listed::Managed(_, State::Missing)]));
        let outcomes = pkgs.install().unwrap();
        assert!(matches!(&outcomes[..], [Outcome::Restored(_)]), "{outcomes:?}");
        assert!(matches!(&pkgs.install().unwrap()[..], [Outcome::Unchanged(_)]));
        assert_eq!(std::fs::read_to_string(&init).unwrap(), "-- old");

        // Branches resolve too, even ones not checked out by the clone.
        run(&src.path(), &["branch", "dev"]);
        commit(&src.path(), &[("plugins/two/init.lua", "-- main only")]);
        let two = pkgs
            .add(&AddRequest {
                source: src.path().to_string_lossy().into_owned(),
                path: Some("plugins/two".into()),
                reference: Some("dev".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(std::fs::read_to_string(pkgs.plugin_dir().join("two/init.lua")).unwrap(), "-- two");
        assert_ne!(two.rev, head(&src.path()));
    }

    #[test]
    fn refuses_bad_installs() {
        let config = tempfile::tempdir().unwrap();
        let pkgs = Packages::new(config.path());

        let empty = repo(&[("README.md", "no plugin here")]);
        let err = add(&pkgs, &empty.path()).unwrap_err();
        assert!(format!("{err:#}").contains("no init.lua"), "{err:#}");
        // Nothing half-installed is left behind.
        assert_eq!(std::fs::read_dir(pkgs.plugin_dir()).unwrap().count(), 0);
        assert!(!pkgs.manifest_path().exists());

        let src = repo(&[("init.lua", "-- managed")]);
        let name = "demo";
        std::fs::write(pkgs.plugin_dir().join(format!("{name}.lua")), "-- mine").unwrap();
        let err = add(&pkgs, &src.path()).unwrap_err();
        assert!(format!("{err:#}").contains("--force"), "{err:#}");
        assert!(matches!(&pkgs.list().unwrap()[..], [Listed::Local(n, _)] if n == name));
        assert!(pkgs.remove(name).is_err());

        let req = AddRequest { source: src.path().to_string_lossy().into_owned(), force: true, ..Default::default() };
        pkgs.add(&req).unwrap();
        assert!(!pkgs.plugin_dir().join(format!("{name}.lua")).exists());

        let err = pkgs
            .add(&AddRequest {
                source: src.path().to_string_lossy().into_owned(),
                reference: Some("nope".into()),
                name: Some("other".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(format!("{err:#}").contains("'nope'"), "{err:#}");
        assert!(pkgs.update(&["missing".into()]).is_err());
    }
}
