//! Docker integration through the `docker` CLI, so it works with Docker
//! Desktop, Podman's docker shim and remote `DOCKER_HOST`s alike.

use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
    pub cpu: Option<String>,
    pub memory: Option<String>,
}

impl Container {
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PsLine {
    #[serde(rename = "ID")]
    id: String,
    names: String,
    image: String,
    state: String,
    status: String,
    #[serde(default)]
    ports: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StatsLine {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "CPUPerc")]
    cpu_perc: String,
    mem_usage: String,
}

fn docker(args: &[&str]) -> Result<String> {
    let out = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("docker is not installed or not on PATH")?;
    if !out.status.success() {
        // Some wrappers (e.g. WSL without Docker integration) explain on stdout.
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let message = if stderr.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { stderr };
        bail!("{}", if message.is_empty() { format!("docker exited with {}", out.status) } else { message });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True when the Docker daemon answers.
pub fn available() -> bool {
    docker(&["version", "--format", "{{.Server.Version}}"]).is_ok()
}

/// All containers, running ones first, with live CPU/memory where running.
pub fn list_containers() -> Result<Vec<Container>> {
    let out = docker(&["ps", "-a", "--no-trunc", "--format", "{{json .}}"])?;
    let mut containers: Vec<Container> = out
        .lines()
        .filter_map(|l| serde_json::from_str::<PsLine>(l).ok())
        .map(|p| Container {
            id: p.id.chars().take(12).collect(),
            name: p.names,
            image: p.image,
            state: p.state,
            status: p.status,
            ports: p.ports,
            cpu: None,
            memory: None,
        })
        .collect();

    if containers.iter().any(Container::is_running) {
        if let Ok(stats) = docker(&["stats", "--no-stream", "--format", "{{json .}}"]) {
            for s in stats.lines().filter_map(|l| serde_json::from_str::<StatsLine>(l).ok()) {
                if let Some(c) = containers.iter_mut().find(|c| s.id.starts_with(&c.id) || c.id.starts_with(&s.id)) {
                    c.cpu = Some(s.cpu_perc);
                    c.memory = Some(s.mem_usage);
                }
            }
        }
    }
    containers.sort_by(|a, b| b.is_running().cmp(&a.is_running()).then_with(|| a.name.cmp(&b.name)));
    Ok(containers)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Remove,
}

impl ContainerAction {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Pause => "pause",
            Self::Unpause => "unpause",
            Self::Remove => "rm",
        }
    }
}

pub fn act(container: &str, action: ContainerAction) -> Result<()> {
    docker(&[action.verb(), container]).map(drop)
}

pub fn logs(container: &str, tail: usize) -> Result<String> {
    let out = Command::new("docker")
        .args(["logs", "--tail", &tail.to_string(), container])
        .stdin(Stdio::null())
        .output()
        .context("failed to run docker")?;
    // Containers log to both streams; show them together.
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(text)
}
