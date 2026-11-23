//! Incus backend (Linux): a system container per node, with /dev/net/tun
//! passed in, provisioned like a Tart VM (Tailscale + microsocks on the
//! container's bridge address).

use anyhow::{bail, Context, Result};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

pub const IMAGE: &str = "images:ubuntu/24.04";

fn incus(args: &[&str]) -> Result<Output> {
    Command::new("incus")
        .args(args)
        .output()
        .context("running incus")
}

pub fn installed() -> bool {
    incus(&["version"]).is_ok_and(|o| o.status.success())
}

pub fn state(name: &str) -> String {
    let Ok(o) = incus(&["list", name, "--format", "json"]) else {
        return "missing".into();
    };
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_default();
    v.as_array()
        .into_iter()
        .flatten()
        .find(|i| i["name"].as_str() == Some(name))
        .and_then(|i| i["status"].as_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "missing".into())
}

pub fn create(name: &str, image: Option<&str>) -> Result<()> {
    let out = incus(&["init", image.unwrap_or(IMAGE), name])?;
    if !out.status.success() {
        bail!(
            "incus init failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let out = incus(&[
        "config",
        "device",
        "add",
        name,
        "tun",
        "unix-char",
        "path=/dev/net/tun",
    ])?;
    if !out.status.success() {
        bail!(
            "adding /dev/net/tun: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    start(name)
}

pub fn start(name: &str) -> Result<()> {
    if state(name) == "missing" {
        bail!("no Incus instance named {name}");
    }
    if state(name) != "running" {
        let out = incus(&["start", name])?;
        if !out.status.success() {
            bail!(
                "incus start {name}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    // Wait for networking inside.
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if ip(name).is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("{name} has no network after 90s")
}

pub fn stop(name: &str) -> Result<()> {
    incus(&["stop", name]).map(|_| ())
}

pub fn delete(name: &str) -> Result<()> {
    incus(&["delete", "--force", name]).map(|_| ())
}

/// IPv4 on the Incus bridge (not the tailscale interface).
pub fn ip(name: &str) -> Option<String> {
    let o = incus(&["list", name, "--format", "json"]).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).ok()?;
    let inst = v
        .as_array()?
        .iter()
        .find(|i| i["name"].as_str() == Some(name))?;
    let nets = inst["state"]["network"].as_object()?;
    nets.iter()
        .filter(|(dev, _)| !dev.starts_with("tailscale") && dev.as_str() != "lo")
        .flat_map(|(_, n)| n["addresses"].as_array().cloned().unwrap_or_default())
        .find(|a| a["family"] == "inet" && a["scope"] == "global")
        .and_then(|a| a["address"].as_str().map(String::from))
}

pub fn exec(name: &str, script: &str) -> Result<Output> {
    incus(&["exec", name, "--", "bash", "-c", script])
}

pub fn shell_command(name: &str) -> Command {
    let mut c = Command::new("incus");
    c.args(["exec", name, "--", "bash", "-l"]);
    c
}
