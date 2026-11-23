//! Where a node runs. Docker is the default everywhere (Linux natively,
//! Docker Desktop on macOS and Windows); Tart (macOS VMs) and Incus (Linux
//! system containers) are alternatives. Each node keeps the backend it was
//! created with.

use crate::config::{Config, Node, SOCKS_PORT};
use crate::{docker, incus, tart};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::process::{Command, Output};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Docker,
    Tart,
    Incus,
}

impl Backend {
    pub fn default_for(cfg: &Config) -> Backend {
        match cfg
            .env
            .get("EXITNODULE_BACKEND")
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("tart") => Backend::Tart,
            Some("incus") => Backend::Incus,
            _ => Backend::Docker,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Backend::Docker => "docker",
            Backend::Tart => "tart",
            Backend::Incus => "incus",
        }
    }

    pub fn installed(self) -> bool {
        match self {
            Backend::Docker => docker::installed(),
            Backend::Tart => tart::installed(),
            Backend::Incus => incus::installed(),
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Backend::Docker => {
                "install Docker (Docker Desktop on macOS/Windows) and make sure it's running"
            }
            Backend::Tart => "macOS only: brew install cirruslabs/cli/tart",
            Backend::Incus => {
                "Linux only: see https://linuxcontainers.org/incus/docs/main/installing/"
            }
        }
    }
}

pub fn state(node: &Node) -> String {
    match node.backend {
        Backend::Docker => docker::state(&node.vm),
        Backend::Tart => tart::state(&node.vm),
        Backend::Incus => incus::state(&node.vm),
    }
}

pub fn running(node: &Node) -> bool {
    state(node) == "running"
}

pub fn start(cfg: &Config, name: &str, node: &Node) -> Result<()> {
    match node.backend {
        Backend::Docker => docker::start(&node.vm),
        Backend::Tart => tart::start(&node.vm, &cfg.log_file(name)),
        Backend::Incus => incus::start(&node.vm),
    }
}

pub fn stop(node: &Node) -> Result<()> {
    match node.backend {
        Backend::Docker => docker::stop(&node.vm),
        Backend::Tart => tart::tart(&["stop", &node.vm]).map(|_| ()),
        Backend::Incus => incus::stop(&node.vm),
    }
}

pub fn delete(node: &Node) -> Result<()> {
    if state(node) == "missing" {
        return Ok(());
    }
    match node.backend {
        Backend::Docker => docker::delete(&node.vm),
        Backend::Tart => {
            if tart::state(&node.vm) == "running" {
                let _ = tart::tart(&["stop", &node.vm]);
            }
            tart::tart(&["delete", &node.vm]).map(|_| ())
        }
        Backend::Incus => incus::delete(&node.vm),
    }
}

/// Run a shell script as root inside the node.
pub fn exec(node: &Node, script: &str) -> Result<Output> {
    match node.backend {
        Backend::Docker => docker::exec(&node.vm, script),
        Backend::Tart => tart::exec(&node.vm, script),
        Backend::Incus => incus::exec(&node.vm, script),
    }
}

pub fn exec_ok(node: &Node, script: &str) -> Result<String> {
    let out = exec(node, script)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = if err.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout)
        } else {
            err
        };
        let err = err.trim();
        let tail = &err[err.len().saturating_sub(800)..];
        bail!("in {}: {tail}", node.vm);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn shell_command(node: &Node) -> Command {
    match node.backend {
        Backend::Docker => docker::shell_command(&node.vm),
        Backend::Tart => {
            let mut c = Command::new("tart");
            c.args(["exec", "-i", "-t", &node.vm, "bash", "-l"]);
            c
        }
        Backend::Incus => incus::shell_command(&node.vm),
    }
}

/// The node's SOCKS proxy as seen from this machine, if it's running.
pub fn proxy(node: &Node) -> Option<(String, u16)> {
    match node.backend {
        Backend::Docker => node.port.map(|p| ("127.0.0.1".to_string(), p)),
        Backend::Tart => tart::ip(&node.vm).map(|ip| (ip, SOCKS_PORT)),
        Backend::Incus => incus::ip(&node.vm).map(|ip| (ip, SOCKS_PORT)),
    }
}

pub fn proxy_url(node: &Node) -> Option<String> {
    proxy(node).map(|(h, p)| format!("socks5h://{h}:{p}"))
}
