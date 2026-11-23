//! Tart backend (macOS only): Linux VMs driven through the `tart` CLI. Commands run inside a VM via
//! Tart's guest agent (`tart exec`), so no SSH keys or passwords.

use anyhow::{bail, Context, Result};

pub const IMAGE: &str = "ghcr.io/cirruslabs/ubuntu:latest";
use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run(args: &[&str]) -> Result<Output> {
    Command::new("tart")
        .args(args)
        .output()
        .context("running tart (brew install cirruslabs/cli/tart)")
}

pub fn tart(args: &[&str]) -> Result<String> {
    let out = run(args)?;
    if !out.status.success() {
        bail!(
            "tart {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `tart clone` shows a progress bar on a first pull; let it through.
pub fn clone(base: &str, vm: &str) -> Result<()> {
    let ok = Command::new("tart")
        .args(["clone", base, vm])
        .status()?
        .success();
    if !ok {
        bail!("tart clone {base} {vm} failed");
    }
    Ok(())
}

pub fn installed() -> bool {
    Command::new("tart")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// "running", "stopped", ... or "missing".
pub fn state(vm: &str) -> String {
    let Ok(out) = run(&["list", "--format", "json"]) else {
        return "missing".into();
    };
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    list.as_array()
        .into_iter()
        .flatten()
        .find(|v| v["Name"].as_str() == Some(vm))
        .and_then(|v| v["State"].as_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "missing".into())
}

pub fn ip(vm: &str) -> Option<String> {
    let out = run(&["ip", vm, "--wait", "5"]).ok()?;
    let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !ip.is_empty()).then_some(ip)
}

/// Run a shell script as root inside the VM.
pub fn exec(vm: &str, script: &str) -> Result<Output> {
    Command::new("tart")
        .args(["exec", vm, "sudo", "bash", "-c", script])
        .output()
        .context("tart exec")
}

/// Boot the VM headless in the background (if needed) and wait until the
/// guest agent answers.
pub fn start(vm: &str, log: &Path) -> Result<()> {
    if state(vm) == "missing" {
        bail!("no VM named {vm}");
    }
    if state(vm) != "running" {
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let f = OpenOptions::new().create(true).append(true).open(log)?;
        let f2 = f.try_clone()?;
        let mut cmd = Command::new("tart");
        cmd.args(["run", "--no-graphics", vm])
            .stdin(Stdio::null())
            .stdout(f)
            .stderr(f2);
        detach(&mut cmd);
        cmd.spawn().context("starting tart run")?;
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if Command::new("tart")
            .args(["exec", vm, "true"])
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!(
        "{vm} didn't come up within 2 minutes (see {})",
        log.display()
    )
}

/// Own process group, so the VM keeps running after we exit and isn't hit
/// by the terminal's Ctrl-C. (Tart is macOS-only; this just keeps other
/// platforms compiling.)
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn detach(_cmd: &mut Command) {}
