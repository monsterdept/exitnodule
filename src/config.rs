//! ~/.config/exitnodule: `env` (credentials and settings), `nodes.json`
//! (the nodes we manage) and `logs/` (VM console output).

use crate::backend::Backend;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub const SOCKS_PORT: u16 = 1080;

pub struct Config {
    pub dir: PathBuf,
    pub env: HashMap<String, String>,
    pub tag: String,
    /// Image override for Tart/Incus (EXITNODULE_BASE).
    pub base: Option<String>,
    pub memory_mb: u32,
    pub cpus: u32,
}

impl Config {
    pub fn load() -> Config {
        let dir = std::env::var_os("EXITNODULE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(default_dir);
        let mut env = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(dir.join("env")) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    env.insert(
                        k.trim().to_string(),
                        v.trim().trim_matches(['"', '\'']).to_string(),
                    );
                }
            }
        }
        // Process environment wins over the file.
        for (k, v) in std::env::vars() {
            if k.starts_with("TS_") || k.starts_with("EXITNODULE_") {
                env.insert(k, v);
            }
        }
        let get = |k: &str, d: &str| env.get(k).cloned().unwrap_or_else(|| d.to_string());
        Config {
            tag: get("TS_TAG", "tag:exitnodule"),
            base: env.get("EXITNODULE_BASE").cloned(),
            memory_mb: get("EXITNODULE_MEMORY_MB", "1024").parse().unwrap_or(1024),
            cpus: get("EXITNODULE_CPUS", "1").parse().unwrap_or(1),
            dir,
            env,
        }
    }

    pub fn env_file(&self) -> PathBuf {
        self.dir.join("env")
    }

    pub fn log_file(&self, name: &str) -> PathBuf {
        self.dir.join("logs").join(format!("{name}.log"))
    }

    fn nodes_file(&self) -> PathBuf {
        self.dir.join("nodes.json")
    }

    pub fn load_nodes(&self) -> Result<Nodes> {
        match std::fs::read_to_string(self.nodes_file()) {
            Ok(text) => serde_json::from_str(&text).context("parsing nodes.json"),
            Err(_) => Ok(Nodes::new()),
        }
    }

    pub fn save_nodes(&self, nodes: &Nodes) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.nodes_file().with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(nodes)? + "\n")?;
        std::fs::rename(tmp, self.nodes_file())?;
        Ok(())
    }
}

pub type Nodes = BTreeMap<String, Node>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Container / VM name.
    pub vm: String,
    /// Name in the tailnet.
    pub hostname: String,
    pub exit_node: String,
    pub created_at: i64,
    /// Nodes from before backends existed were Tart VMs.
    #[serde(default = "legacy_backend")]
    pub backend: Backend,
    /// Host port of the SOCKS proxy (Docker nodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

fn legacy_backend() -> Backend {
    Backend::Tart
}

pub fn vm_name(name: &str) -> String {
    format!("exitnodule-{name}")
}

/// ~/.config/exitnodule on macOS and Linux, %APPDATA%\exitnodule on Windows.
fn default_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("exitnodule");
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("exitnodule")
}
