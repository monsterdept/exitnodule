//! exitnodule: nodes (Docker containers by default; Tart VMs or Incus
//! containers optionally) that each join your tailnet with a different exit
//! node, e.g. Mullvad locations. Each gives you a SOCKS proxy on this
//! machine, a shell, and (Docker) a way to run workloads in its network.
//!
//! An exit node applies to a whole Tailscale device. Each node runs its own
//! tailscaled in its own network namespace, so it is its own device: one
//! program can use several exit nodes at once, through several nodes.

mod backend;
mod config;
mod docker;
mod incus;
mod tailscale;
mod tart;

use anyhow::{bail, Context, Result};
use backend::Backend;
use clap::{Parser, Subcommand};
use config::{vm_name, Config, Node, SOCKS_PORT};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tailscale::Api;

/// Docker nodes publish their SOCKS proxy on 127.0.0.1 from this port up.
const FIRST_DOCKER_PORT: u16 = 10800;

#[derive(Parser)]
#[command(
    version,
    about = "Nodes that each leave via a different Tailscale exit node"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a node, join it to the tailnet and point it at an exit node.
    Init {
        #[arg(long)]
        name: String,
        /// e.g. us-sfo-wg-001.mullvad.ts.net (see `exitnodule exit-nodes`)
        #[arg(long)]
        exit_node: String,
        /// docker (default), tart (macOS VMs) or incus (Linux containers).
        #[arg(long, value_enum)]
        backend: Option<Backend>,
        /// Image for tart/incus nodes (default per backend).
        #[arg(long)]
        base: Option<String>,
    },
    /// Switch a node to another exit node.
    SetExit { name: String, exit_node: String },
    /// Start a node ("all" for every node).
    Start { name: String },
    /// Stop a node ("all" for every node).
    Stop { name: String },
    /// Interactive shell inside a node; its traffic leaves via the exit node.
    Shell { name: String },
    /// Run a container in a Docker node's network, so it leaves via that
    /// node's exit node (no proxy needed).
    Run {
        name: String,
        /// Image to run.
        #[arg(long, conflicts_with = "dockerfile")]
        image: Option<String>,
        /// Build this Dockerfile (its directory is the build context) and run it.
        #[arg(long)]
        dockerfile: Option<String>,
        /// Extra `docker run` flags, e.g. --docker-arg=-v --docker-arg=./data:/data
        #[arg(long = "docker-arg", allow_hyphen_values = true)]
        docker_args: Vec<String>,
        /// Command and arguments for the container.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Delete the node and remove it from the tailnet.
    Destroy { name: String },
    /// Nodes, their backend, state, exit node and proxy (no probing; fast).
    #[command(alias = "ls")]
    List,
    /// State, exit node, public address and proxy of every node.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// SOCKS URLs of running nodes, comma-separated.
    Proxies,
    /// List the exit nodes your tailnet offers (optional filter, e.g. "us-").
    ExitNodes { filter: Option<String> },
    /// Check credentials, tailnet policy and tools.
    Doctor,
}

fn say(msg: &str) {
    println!("==> {msg}");
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("exitnodule: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let cfg = Config::load();
    match cli.cmd {
        Cmd::Init {
            name,
            exit_node,
            backend,
            base,
        } => init(
            &cfg,
            &name,
            &exit_node,
            backend.unwrap_or_else(|| Backend::default_for(&cfg)),
            base,
        ),
        Cmd::SetExit { name, exit_node } => set_exit(&cfg, &name, &exit_node),
        Cmd::Start { name } => {
            for (n, node) in selected(&cfg, &name)? {
                say(&format!("Starting {n}"));
                backend::start(&cfg, &n, &node)?;
                println!("    {n}: {}", describe_egress(&egress(&node)));
            }
            Ok(())
        }
        Cmd::Stop { name } => {
            for (n, node) in selected(&cfg, &name)? {
                if backend::running(&node) {
                    say(&format!("Stopping {n}"));
                    backend::stop(&node)?;
                }
            }
            Ok(())
        }
        Cmd::Shell { name } => {
            let node = get_node(&cfg, &name)?;
            backend::start(&cfg, &name, &node)?;
            replace_process(backend::shell_command(&node))
        }
        Cmd::Run {
            name,
            image,
            dockerfile,
            docker_args,
            args,
        } => run_workload(&cfg, &name, image, dockerfile, &docker_args, &args),
        Cmd::Destroy { name } => destroy(&cfg, &name),
        Cmd::List => list(&cfg),
        Cmd::Status { json } => status(&cfg, json),
        Cmd::Proxies => {
            let urls: Vec<String> = cfg
                .load_nodes()?
                .values()
                .filter(|n| backend::running(n))
                .filter_map(backend::proxy_url)
                .collect();
            println!("{}", urls.join(","));
            Ok(())
        }
        Cmd::ExitNodes { filter } => exit_nodes(&cfg, filter.as_deref()),
        Cmd::Doctor => doctor(&cfg),
    }
}

fn get_node(cfg: &Config, name: &str) -> Result<Node> {
    cfg.load_nodes()?
        .get(name)
        .cloned()
        .with_context(|| format!("unknown node {name}"))
}

fn selected(cfg: &Config, name: &str) -> Result<Vec<(String, Node)>> {
    let nodes = cfg.load_nodes()?;
    if name == "all" {
        return Ok(nodes.into_iter().collect());
    }
    Ok(vec![(name.to_string(), get_node(cfg, name)?)])
}

/// exec() on Unix (so the shell owns the terminal); spawn-and-wait elsewhere.
fn replace_process(mut cmd: Command) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        bail!("{err}")
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

// --- inside Tart / Incus nodes ------------------------------------------------------

/// Installs Tailscale and a SOCKS proxy bound to the node's private address
/// (reachable from this machine, not from the tailnet).
const PROVISION: &str = r#"
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
command -v curl >/dev/null || { apt-get update -qq; apt-get install -y -qq curl; }
if ! command -v tailscale >/dev/null; then
  curl -fsSL https://tailscale.com/install.sh | sh
fi
if ! command -v microsocks >/dev/null; then
  apt-get update -qq
  apt-get install -y -qq microsocks
fi
cat > /usr/local/bin/exitnodule-socks <<'EOF'
#!/bin/sh
ip=$(ip -4 -o addr show scope global | awk '$2 !~ /^tailscale/ {sub(/\/.*/, "", $4); print $4; exit}')
exec /usr/bin/microsocks -i "$ip" -p __PORT__
EOF
chmod +x /usr/local/bin/exitnodule-socks
cat > /etc/systemd/system/exitnodule-socks.service <<'EOF'
[Unit]
Description=exitnodule SOCKS proxy
After=network-online.target tailscaled.service
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/exitnodule-socks
Restart=always
RestartSec=2
DynamicUser=yes

[Install]
WantedBy=multi-user.target
EOF
cat > /etc/systemd/system/exitnodule-lan.service <<'EOF'
[Unit]
Description=exitnodule: keep the local network off the exit node
After=network-online.target tailscaled.service
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c '__LAN_ROUTE__'

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now exitnodule-socks
systemctl enable exitnodule-lan
"#;

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// --- egress: checked from this machine through the node's own proxy ----------------

fn egress(node: &Node) -> Value {
    let Some((host, port)) = backend::proxy(node) else {
        return Value::Null;
    };
    let Ok(proxy) = ureq::Proxy::new(format!("socks5://{host}:{port}")) else {
        return Value::Null;
    };
    let agent = ureq::AgentBuilder::new()
        .proxy(proxy)
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build();
    for url in ["https://am.i.mullvad.net/json", "https://ipinfo.io/json"] {
        if let Ok(v) = agent
            .get(url)
            .call()
            .map_err(|_| ())
            .and_then(|r| r.into_json::<Value>().map_err(|_| ()))
        {
            return v;
        }
    }
    Value::Null
}

fn describe_egress(e: &Value) -> String {
    if e.is_null() {
        return "unknown: proxy unreachable from this machine (try `exitnodule start <name>`)"
            .into();
    }
    let ip = e["ip"].as_str().unwrap_or("?");
    let place: Vec<&str> = [e["city"].as_str(), e["country"].as_str()]
        .into_iter()
        .flatten()
        .collect();
    let via = if e["mullvad_exit_ip"].as_bool() == Some(true) {
        " (Mullvad)"
    } else {
        ""
    };
    format!("{ip} {}{via}", place.join(", ")).trim().to_string()
}

// --- commands --------------------------------------------------------------------

fn init(
    cfg: &Config,
    name: &str,
    exit_node: &str,
    backend: Backend,
    base: Option<String>,
) -> Result<()> {
    let mut nodes = cfg.load_nodes()?;
    if let Some(n) = nodes.get(name) {
        bail!("{name} already exists ({}); destroy it first", n.exit_node);
    }
    if !backend.installed() {
        bail!(
            "{} isn't available: {}",
            backend.name(),
            backend.install_hint()
        );
    }
    let vm = vm_name(name);
    let mut node = Node {
        vm: vm.clone(),
        hostname: vm.clone(),
        exit_node: exit_node.to_string(),
        created_at: now(),
        backend,
        port: None,
    };
    if backend::state(&node) != "missing" {
        bail!(
            "a {} instance named {vm} already exists; remove it or pick another name",
            backend.name()
        );
    }
    let api = Api::new(cfg)?; // fail on credentials before building anything
    let key = api.create_auth_key(name)?;
    let base = base.or_else(|| cfg.base.clone());

    match backend {
        Backend::Docker => {
            let port = free_port(&nodes)?;
            node.port = Some(port);
            say(&format!("Starting container {vm} ({})", docker::IMAGE));
            docker::create(&vm, &vm, &key, &cfg.tag, port)?;
        }
        Backend::Tart | Backend::Incus => {
            if backend == Backend::Tart {
                let base = base.as_deref().unwrap_or(tart::IMAGE);
                say(&format!("Creating VM {vm} from {base}"));
                tart::clone(base, &vm)?;
                tart::tart(&[
                    "set",
                    &vm,
                    "--cpu",
                    &cfg.cpus.to_string(),
                    "--memory",
                    &cfg.memory_mb.to_string(),
                ])?;
                tart::start(&vm, &cfg.log_file(name))?;
            } else {
                say(&format!(
                    "Creating container {vm} from {}",
                    base.as_deref().unwrap_or(incus::IMAGE)
                ));
                incus::create(&vm, base.as_deref())?;
            }
            nodes.insert(name.to_string(), node.clone());
            cfg.save_nodes(&nodes)?;
            say("Installing Tailscale and the SOCKS proxy");
            backend::exec_ok(
                &node,
                &PROVISION
                    .replace("__PORT__", &SOCKS_PORT.to_string())
                    .replace("__LAN_ROUTE__", &backend::LAN_ROUTE.replace('\'', r"'\''")),
            )?;
            say(&format!("Joining the tailnet as {vm} ({})", cfg.tag));
            let up = format!(
                "tailscale up --auth-key={} --hostname={} --advertise-tags={} --reset",
                sh_quote(&key),
                sh_quote(&vm),
                sh_quote(&cfg.tag)
            );
            backend::exec_ok(&node, &up).with_context(|| {
                format!(
                    "tailscale up failed; the node is left in place (`exitnodule destroy {name}`)"
                )
            })?;
        }
    }
    nodes.insert(name.to_string(), node.clone());
    cfg.save_nodes(&nodes)?;
    point_at_exit(cfg, name, &node, exit_node)?;

    say("Checking where its traffic comes out");
    println!("    {name}: {}", describe_egress(&egress(&node)));
    if let Some(url) = backend::proxy_url(&node) {
        println!("    proxy: {url}");
    }
    Ok(())
}

/// The next host port not used by another Docker node or anything else.
fn free_port(nodes: &config::Nodes) -> Result<u16> {
    let taken: Vec<u16> = nodes.values().filter_map(|n| n.port).collect();
    (FIRST_DOCKER_PORT..FIRST_DOCKER_PORT + 1000)
        .find(|p| !taken.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .context("no free port for the SOCKS proxy")
}

/// Exit node names only resolve once the netmap has arrived, so retry.
fn point_at_exit(cfg: &Config, name: &str, node: &Node, exit_node: &str) -> Result<()> {
    say(&format!("Setting exit node {exit_node}"));
    let cmd = format!(
        "tailscale set --exit-node={} --exit-node-allow-lan-access",
        sh_quote(exit_node)
    );
    let mut last = String::new();
    for _ in 0..30 {
        let out = backend::exec(node, &cmd)?;
        if out.status.success() {
            return backend::ensure_lan_route(node);
        }
        last = String::from_utf8_lossy(&out.stderr).trim().to_string();
        std::thread::sleep(Duration::from_secs(2));
    }
    bail!(
        "couldn't use exit node {exit_node}: {last}\nFor Mullvad, check `exitnodule doctor` (the policy must grant \
         Mullvad to {}) and that the add-on has a free device slot. Then `exitnodule set-exit {name} {exit_node}`, \
         or `exitnodule destroy {name}`.",
        cfg.tag
    )
}

fn set_exit(cfg: &Config, name: &str, exit_node: &str) -> Result<()> {
    let mut nodes = cfg.load_nodes()?;
    let node = nodes
        .get(name)
        .cloned()
        .with_context(|| format!("unknown node {name}"))?;
    backend::start(cfg, name, &node)?;
    point_at_exit(cfg, name, &node, exit_node)?;
    if let Some(n) = nodes.get_mut(name) {
        n.exit_node = exit_node.to_string();
    }
    cfg.save_nodes(&nodes)?;
    println!("    {name}: {}", describe_egress(&egress(&node)));
    Ok(())
}

fn run_workload(
    cfg: &Config,
    name: &str,
    image: Option<String>,
    dockerfile: Option<String>,
    docker_args: &[String],
    args: &[String],
) -> Result<()> {
    let node = get_node(cfg, name)?;
    if node.backend != Backend::Docker {
        bail!(
            "`run` needs a Docker node; {name} is {} (use its proxy or `exitnodule shell`)",
            node.backend.name()
        );
    }
    backend::start(cfg, name, &node)?;
    let image = match (image, dockerfile) {
        (Some(i), _) => i,
        (None, Some(df)) => {
            let path = Path::new(&df);
            let context = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let tag = format!("exitnodule-work-{name}");
            say(&format!("Building {df} as {tag}"));
            let ok = Command::new("docker")
                .args(["build", "-t", &tag, "-f", &df])
                .arg(context)
                .status()
                .context("docker build")?
                .success();
            if !ok {
                bail!("docker build failed");
            }
            tag
        }
        (None, None) => bail!("give --image or --dockerfile"),
    };
    let net = format!("container:{}", node.vm);
    let mut cmd = Command::new("docker");
    cmd.args(["run", "--rm", "--network", &net]);
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        cmd.arg("-it");
    }
    cmd.args(docker_args).arg(&image).args(args);
    replace_process(cmd)
}

fn destroy(cfg: &Config, name: &str) -> Result<()> {
    let mut nodes = cfg.load_nodes()?;
    let node = nodes
        .get(name)
        .cloned()
        .with_context(|| format!("unknown node {name}"))?;
    if backend::state(&node) != "missing" {
        say(&format!("Deleting {} {}", node.backend.name(), node.vm));
        backend::delete(&node)?;
    }
    say(&format!("Removing {} from the tailnet", node.hostname));
    println!(
        "    removed {} device(s)",
        Api::new(cfg)?.delete_devices(&node.hostname)?
    );
    nodes.remove(name);
    cfg.save_nodes(&nodes)
}

fn list(cfg: &Config) -> Result<()> {
    let nodes = cfg.load_nodes()?;
    if nodes.is_empty() {
        println!("No nodes. Create one with `exitnodule init --name node1 --exit-node <host>`.");
        return Ok(());
    }
    println!(
        "{:<10} {:<7} {:<9} {:<34} PROXY",
        "NAME", "BACKEND", "STATE", "EXIT NODE"
    );
    for (name, node) in &nodes {
        let state = backend::state(node);
        let proxy = if state == "running" {
            backend::proxy_url(node).unwrap_or_default()
        } else {
            String::new()
        };
        println!(
            "{name:<10} {:<7} {state:<9} {:<34} {proxy}",
            node.backend.name(),
            node.exit_node
        );
    }
    Ok(())
}

fn status(cfg: &Config, as_json: bool) -> Result<()> {
    let nodes = cfg.load_nodes()?;
    if nodes.is_empty() {
        println!("No nodes. Create one with `exitnodule init --name node1 --exit-node <host>`.");
        return Ok(());
    }
    // Probe every node at once.
    let reports: Vec<Value> = std::thread::scope(|s| {
        let handles: Vec<_> = nodes
            .iter()
            .map(|(name, node)| s.spawn(move || report(name, node)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Value::Null))
            .collect()
    });
    if as_json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }
    for r in &reports {
        let name = r["name"].as_str().unwrap_or("?");
        let state = r["state"].as_str().unwrap_or("?");
        println!(
            "{name:<10} {state:<8} {:<7} via {}",
            r["backend"].as_str().unwrap_or("?"),
            r["exit_node"].as_str().unwrap_or("?")
        );
        if state == "running" {
            let exit = if r["exit_active"].as_bool() == Some(true) {
                "exit node up".to_string()
            } else {
                format!(
                    "exit node DOWN (tailscale: {})",
                    r["tailscale"].as_str().unwrap_or("?")
                )
            };
            println!(
                "           {exit}; out as {}",
                r["egress"].as_str().unwrap_or("?")
            );
            println!("           proxy {}", r["proxy"].as_str().unwrap_or("?"));
        }
    }
    Ok(())
}

fn report(name: &str, node: &Node) -> Value {
    let state = backend::state(node);
    let mut r = serde_json::json!({
        "name": name, "backend": node.backend.name(), "exit_node": node.exit_node, "state": state,
    });
    if state == "running" {
        r["proxy"] = backend::proxy_url(node).into();
        let ts: Value = backend::exec(node, "tailscale status --json")
            .ok()
            .and_then(|o| serde_json::from_slice(&o.stdout).ok())
            .unwrap_or(Value::Null);
        r["tailscale"] = ts["BackendState"].as_str().unwrap_or("not running").into();
        r["exit_active"] = (ts["ExitNodeStatus"]["Online"].as_bool() == Some(true)).into();
        r["egress"] = describe_egress(&egress(node)).into();
    }
    r
}

/// The Tailscale CLI on this machine, if any.
fn host_tailscale() -> Option<Command> {
    let mac = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
    if cfg!(target_os = "macos") && Path::new(mac).exists() {
        return Some(Command::new(mac));
    }
    Command::new("tailscale")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
        .then(|| Command::new("tailscale"))
}

fn exit_nodes(cfg: &Config, filter: Option<&str>) -> Result<()> {
    let mut text = None;
    if let Some(mut ts) = host_tailscale() {
        let out = ts.args(["exit-node", "list"]).output()?;
        if out.status.success() {
            text = Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    if text.is_none() {
        let node = cfg.load_nodes()?.into_values().find(backend::running);
        let Some(node) = node else {
            bail!("no Tailscale on this machine and no running node to ask")
        };
        text = Some(backend::exec_ok(&node, "tailscale exit-node list")?);
    }
    let text = text.unwrap_or_default();
    let mut lines = text.lines();
    if let Some(header) = lines.next() {
        println!("{header}");
    }
    for l in lines {
        if filter.is_none_or(|f| l.to_lowercase().contains(&f.to_lowercase())) {
            println!("{l}");
        }
    }
    Ok(())
}

fn doctor(cfg: &Config) -> Result<()> {
    let mut ok = true;
    let mut check = |label: &str, good: bool, hint: &str| {
        println!("[{}] {label}", if good { "ok" } else { "!!" });
        if !good && !hint.is_empty() {
            println!("     {hint}");
        }
        ok &= good;
        good
    };
    let backend = Backend::default_for(cfg);
    check(
        &format!("{} available (default backend)", backend.name()),
        backend.installed(),
        backend.install_hint(),
    );
    let creds = check(
        &format!("Tailscale credentials in {}", cfg.env_file().display()),
        Api::has_credentials(cfg),
        "Admin console > Settings > Keys > Generate access token, then TS_API_KEY=tskey-api-...",
    );
    if creds {
        match Api::new(cfg).and_then(|api| api.check_devices().map(|_| api)) {
            Ok(api) => {
                check("API token works", true, "");
                let pol = api.policy().unwrap_or(Value::Null);
                check(
                    &format!("policy defines {} in tagOwners", cfg.tag),
                    pol["tagOwners"].get(&cfg.tag).is_some(),
                    &format!(
                        r#"add  "tagOwners": {{"{}": ["autogroup:admin"]}}"#,
                        cfg.tag
                    ),
                );
                let granted = pol["nodeAttrs"].as_array().into_iter().flatten().any(|na| {
                    na["target"]
                        .as_array()
                        .is_some_and(|t| t.iter().any(|x| x.as_str() == Some(&cfg.tag)))
                        && na["attr"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("mullvad")))
                });
                check(
                    &format!("policy grants Mullvad to {}", cfg.tag),
                    granted,
                    &format!(
                        r#"add  "nodeAttrs": [{{"target": ["{}"], "attr": ["mullvad"]}}]"#,
                        cfg.tag
                    ),
                );
            }
            Err(e) => {
                check("API token works", false, &format!("{e:#}"));
            }
        }
    }
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
