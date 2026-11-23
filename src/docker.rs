//! Docker backend: each node is a container from Tailscale's official image
//! (Linux natively; Docker Desktop on macOS and Windows). Tailscale runs in
//! kernel mode with the TUN device, so everything in the container, including
//! `exitnodule shell`, leaves via the exit node. Its built-in SOCKS5 server is
//! published on 127.0.0.1 only.

use anyhow::{bail, Context, Result};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

pub const IMAGE: &str = "tailscale/tailscale:stable";
pub const CONTAINER_SOCKS_PORT: u16 = 1080;

fn docker(args: &[&str]) -> Result<Output> {
    Command::new("docker")
        .args(args)
        .output()
        .context("running docker (is Docker installed and running?)")
}

pub fn installed() -> bool {
    docker(&["version", "--format", "{{.Server.Version}}"]).is_ok_and(|o| o.status.success())
}

pub fn state(name: &str) -> String {
    match docker(&["inspect", "-f", "{{.State.Status}}", name]) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "missing".into(),
    }
}

fn volume(name: &str) -> String {
    format!("{name}-state")
}

/// Create and start the container; it joins the tailnet with `auth_key`.
pub fn create(name: &str, hostname: &str, auth_key: &str, tag: &str, host_port: u16) -> Result<()> {
    let publish = format!("127.0.0.1:{host_port}:{CONTAINER_SOCKS_PORT}");
    let vol = format!("{}:/var/lib/tailscale", volume(name));
    let envs = [
        format!("TS_AUTHKEY={auth_key}"),
        format!("TS_HOSTNAME={hostname}"),
        "TS_STATE_DIR=/var/lib/tailscale".to_string(),
        "TS_USERSPACE=false".to_string(),
        "TS_AUTH_ONCE=true".to_string(),
        format!("TS_SOCKS5_SERVER=:{CONTAINER_SOCKS_PORT}"),
        format!("TS_EXTRA_ARGS=--advertise-tags={tag}"),
    ];
    let mut args: Vec<&str> = vec![
        "run",
        "-d",
        "--name",
        name,
        "--hostname",
        hostname,
        "--restart",
        "unless-stopped",
        "--cap-add",
        "NET_ADMIN",
        "--cap-add",
        "NET_RAW",
        "--device",
        "/dev/net/tun",
        "-v",
        &vol,
        "-p",
        &publish,
    ];
    for e in &envs {
        args.push("-e");
        args.push(e);
    }
    args.push(IMAGE);
    let out = docker(&args)?;
    if !out.status.success() {
        bail!(
            "docker run failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    wait_running(name)
}

/// Wait until tailscaled inside reports it's connected.
pub fn wait_running(name: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Ok(o) = exec(name, "tailscale status --json") {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                if v["BackendState"] == "Running" {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("{name}: tailscale didn't connect within 90s (docker logs {name})")
}

pub fn start(name: &str) -> Result<()> {
    if state(name) == "missing" {
        bail!("no container named {name}");
    }
    if state(name) != "running" {
        let out = docker(&["start", name])?;
        if !out.status.success() {
            bail!(
                "docker start {name}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    wait_running(name)
}

pub fn stop(name: &str) -> Result<()> {
    docker(&["stop", name]).map(|_| ())
}

pub fn delete(name: &str) -> Result<()> {
    docker(&["rm", "-f", name])?;
    docker(&["volume", "rm", "-f", &volume(name)])?;
    Ok(())
}

pub fn exec(name: &str, script: &str) -> Result<Output> {
    docker(&["exec", name, "sh", "-c", script])
}

pub fn shell_command(name: &str) -> Command {
    let mut c = Command::new("docker");
    c.args(["exec", "-it", name, "sh", "-l"]);
    c
}
