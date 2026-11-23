# exitnodule

Nodes that each join your tailnet with a different exit node (e.g. Mullvad
locations), each giving this machine a SOCKS proxy that leaves from there. Use
several at once from one program, open a shell "from" a city, or run a container
whose traffic leaves via a node.

```
exitnodule init --name node1 --exit-node us-sfo-wg-001.mullvad.ts.net
exitnodule list                    # nodes, backend, state, exit node, proxy (fast, no probing)
exitnodule status                  # state, exit node, public address, proxy URL (--json)
exitnodule proxies                 # socks5h://127.0.0.1:10800,... for running nodes
exitnodule shell node1             # shell inside; its traffic leaves via the exit node
exitnodule run node1 --image curlimages/curl -- -s https://am.i.mullvad.net/json
exitnodule run node1 --dockerfile ./job/Dockerfile -- --some-flag
exitnodule set-exit node1 de-dus-wg-101.mullvad.ts.net
exitnodule stop node1              # or: stop all / start all
exitnodule destroy node1           # deletes the node and removes it from the tailnet
exitnodule exit-nodes us-          # exit nodes your tailnet offers (filter optional)
exitnodule doctor                  # check tools, credentials and tailnet policy
```

## Why nodes

An exit node applies to a whole Tailscale device: your laptop can use one at a
time, for all its traffic. Each exitnodule node runs its own `tailscaled` in its
own network namespace, so it's its own device with its own exit node, and nothing
else on the machine is affected.

## Backends

| Backend | Where | What a node is | Proxy |
|---|---|---|---|
| `docker` (default) | Linux; macOS and Windows with Docker Desktop | a `tailscale/tailscale` container in kernel mode (TUN) | Tailscale's SOCKS5 server, published on `127.0.0.1:10800+` |
| `tart` | macOS | a [Tart](https://tart.run) Ubuntu VM (1 CPU, 1 GB) | microsocks on the VM's private address |
| `incus` | Linux | an [Incus](https://linuxcontainers.org/incus/) Ubuntu system container with `/dev/net/tun` | microsocks on the container's bridge address |

Pick with `--backend` on `init` or `EXITNODULE_BACKEND` in the env file. Each node
keeps the backend it was created with. Proxies are reachable from this machine
only, not from the tailnet. `run` (Docker nodes) starts a container in the node's
network namespace (`--network container:…`), so the workload needs no proxy at all.

## Setup (once)

1. **A backend:** Docker running, or Tart / Incus.
2. **API credentials:** an API access token (admin console → Settings → Keys →
   Generate access token; expires after at most 90 days) or an OAuth client with the
   `auth_keys` and `devices:core` scopes (plus `policy_file:read` for `doctor`) that
   owns the tag. In the env file (`~/.config/exitnodule/env`, or
   `%APPDATA%\exitnodule\env` on Windows):

   ```
   TS_API_KEY=tskey-api-...
   # or
   TS_OAUTH_CLIENT_ID=...
   TS_OAUTH_CLIENT_SECRET=tskey-client-...
   ```

3. **Tailnet policy** (admin console → Access controls): own the tag, and for
   Mullvad, grant it:

   ```jsonc
   "tagOwners": {
     "tag:exitnodule": ["autogroup:admin"],
   },
   "nodeAttrs": [
     {"target": ["tag:exitnodule"], "attr": ["mullvad"]},
   ],
   ```

   Merge these into what's there; `exitnodule doctor` should then be all `[ok]`.

4. **Install:** `brew install monsterdept/tap/exitnodule` (macOS and Linux), an
   archive from [Releases](https://github.com/monsterdept/exitnodule/releases), or
   from source: `cargo install --path .` (or `just install`).

`init` creates a single-use, pre-approved auth key tagged `tag:exitnodule` through the
Tailscale API, joins the node, then sets the exit node with
`--exit-node-allow-lan-access` (so this machine can still reach the proxy).
Because that exemption can drop out of a Linux node's routing table while it
runs, each node also gets its own routing rule that keeps its local subnet off
the exit node, reapplied on every start and exit-node change. `destroy` deletes
the device through the API.

### A note on Mullvad accounting

Mullvad access granted by the policy rule above works (`am.i.mullvad.net` reports a
Mullvad exit from every node), but those devices don't appear on the admin console's
Mullvad page, in Billing, or on their machine pages. How Tailscale counts or bills
them is unclear. To be safe, keep the number of policy-granted nodes within the
add-on's free device slots.

## Settings

Set these in the env file, or in your shell, which wins over the file:

| Variable | Default | What it does |
|---|---|---|
| `TS_TAG` | `tag:exitnodule` | Tag given to every node |
| `EXITNODULE_BACKEND` | `docker` | Backend for `init` when `--backend` isn't given |
| `EXITNODULE_BASE` | per backend | Image for Tart and Incus nodes |
| `EXITNODULE_MEMORY_MB` | `1024` | Memory per Tart VM |
| `EXITNODULE_CPUS` | `1` | CPUs per Tart VM |
| `EXITNODULE_HOME` | `~/.config/exitnodule` | Where the env file and state live (shell only) |

Alongside the env file: `nodes.json` (the nodes exitnodule manages) and `logs/`
(Tart VM consoles).

## Building

```
cargo build --release
cargo zigbuild --release --target x86_64-unknown-linux-gnu    # cross-compile (needs zig)
```

CI builds and lints on macOS, Linux and Windows.

## License

exitnodule is free software under the [GNU General Public License, version 3 or later](LICENSE).
