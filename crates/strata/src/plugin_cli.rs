//! `strata plugin …`: install, update and remove plugins from git
//! repositories (see `strata_plugin::package`).

use strata_plugin::package::{AddRequest, Listed, Outcome, Package, Packages, State};

#[derive(clap::Subcommand, Debug)]
pub enum PluginCommand {
    /// Install a plugin from a git repository, or reinstall it with new
    /// settings.
    Add {
        /// `owner/repo` on GitHub, a git URL or a local repository.
        source: String,
        /// Install under this name instead of the repository's.
        #[arg(long)]
        name: Option<String>,
        /// Branch, tag or commit to install and follow. Default: the
        /// repository's default branch.
        #[arg(long = "ref", value_name = "REF")]
        reference: Option<String>,
        /// Folder inside the repository that holds the plugin's init.lua.
        #[arg(long, value_name = "DIR")]
        path: Option<String>,
        /// Replace a plugin of the same name that was not installed with
        /// `strata plugin`.
        #[arg(long)]
        force: bool,
    },
    /// Delete installed plugins.
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
    },
    /// Move plugins to the newest commit of the branch or tag they follow;
    /// all of them when no names are given.
    Update {
        #[arg(value_name = "NAME")]
        names: Vec<String>,
    },
    /// Install the plugins in plugins.toml at the commits recorded there,
    /// e.g. on a new machine.
    Install,
    /// List installed plugins.
    #[command(visible_alias = "ls")]
    List,
}

/// Runs the command and returns the exit code.
pub fn run(cmd: PluginCommand) -> i32 {
    let packages = Packages::new(&strata_config::config_dir());
    let result = match cmd {
        PluginCommand::Add { source, name, reference, path, force } => {
            packages.add(&AddRequest { source, name, path, reference, force }).map(|pkg| {
                println!("installed {} {} from {}", pkg.name, pkg.short_rev(), pkg.origin());
                println!("restart strata to load it");
                0
            })
        }
        PluginCommand::Remove { names } => {
            let mut code = 0;
            for name in names {
                match packages.remove(&name) {
                    Ok(pkg) => println!("removed {}", pkg.name),
                    Err(e) => {
                        eprintln!("strata: {e:#}");
                        code = 1;
                    }
                }
            }
            Ok(code)
        }
        PluginCommand::Update { names } => packages.update(&names).map(|o| report(&o, "no plugins to update")),
        PluginCommand::Install => packages.install().map(|o| report(&o, "no plugins in plugins.toml")),
        PluginCommand::List => packages.list().map(|rows| {
            list(&packages, &rows);
            0
        }),
    };
    result.unwrap_or_else(|e| {
        eprintln!("strata: {e:#}");
        1
    })
}

/// One line per plugin; nonzero when any failed.
fn report(outcomes: &[Outcome], none: &str) -> i32 {
    if outcomes.is_empty() {
        println!("{none}");
    }
    let mut code = 0;
    for outcome in outcomes {
        match outcome {
            Outcome::Unchanged(pkg) => println!("{}: up to date ({})", pkg.name, pkg.short_rev()),
            Outcome::Restored(pkg) => println!("{}: installed {}", pkg.name, pkg.short_rev()),
            Outcome::Changed { from, to } => {
                println!("{}: {} -> {}", to.name, &from[..from.len().min(7)], to.short_rev())
            }
            Outcome::Failed(name, e) => {
                eprintln!("{name}: {e:#}");
                code = 1;
            }
        }
    }
    code
}

fn list(packages: &Packages, rows: &[Listed]) {
    if rows.is_empty() {
        println!("no plugins in {}", packages.plugin_dir().display());
        println!("install one with `strata plugin add <owner/repo>`");
        return;
    }
    let disabled = strata_config::Config::load(None).map(|c| c.plugins.disabled).unwrap_or_default();
    let name_of = |row: &Listed| match row {
        Listed::Managed(pkg, _) => pkg.name.clone(),
        Listed::Local(name, _) => name.clone(),
    };
    let width = rows.iter().map(|r| name_of(r).chars().count()).max().unwrap_or(0);
    for row in rows {
        let name = name_of(row);
        let mut line = match row {
            Listed::Managed(pkg, state) => {
                format!("{name:<width$}  {}  {}{}", pkg.short_rev(), describe(pkg), note(*state))
            }
            Listed::Local(_, path) => format!("{name:<width$}  {:<7}  {}", "local", path.display()),
        };
        if disabled.contains(&name) {
            line.push_str("  (disabled in config.toml)");
        }
        println!("{line}");
    }
}

fn describe(pkg: &Package) -> String {
    match &pkg.reference {
        Some(r) => format!("{} @ {r}", pkg.origin()),
        None => pkg.origin(),
    }
}

fn note(state: State) -> &'static str {
    match state {
        State::Installed => "",
        State::Missing => "  (missing: run `strata plugin install`)",
        State::Stale => "  (other files than plugins.toml records: run `strata plugin install`)",
    }
}
