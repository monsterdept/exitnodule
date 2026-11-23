//! The Tailscale API v2: auth keys for new nodes, device cleanup, and a
//! read of the policy file for `doctor`.

use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://api.tailscale.com/api/v2";

pub struct Api {
    agent: ureq::Agent,
    token: String,
    tag: String,
}

impl Api {
    pub fn has_credentials(cfg: &Config) -> bool {
        cfg.env.contains_key("TS_API_KEY")
            || (cfg.env.contains_key("TS_OAUTH_CLIENT_ID")
                && cfg.env.contains_key("TS_OAUTH_CLIENT_SECRET"))
    }

    pub fn new(cfg: &Config) -> Result<Api> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        let token = if let Some(k) = cfg.env.get("TS_API_KEY") {
            k.clone()
        } else if let (Some(id), Some(secret)) = (
            cfg.env.get("TS_OAUTH_CLIENT_ID"),
            cfg.env.get("TS_OAUTH_CLIENT_SECRET"),
        ) {
            let res: Value = agent
                .post(&format!("{API}/oauth/token"))
                .send_form(&[
                    ("client_id", id),
                    ("client_secret", secret),
                    ("grant_type", "client_credentials"),
                ])
                .map_err(http_err)?
                .into_json()?;
            res["access_token"]
                .as_str()
                .context("no access_token in OAuth response")?
                .to_string()
        } else {
            bail!(
                "no Tailscale credentials: set TS_API_KEY (or TS_OAUTH_CLIENT_ID + TS_OAUTH_CLIENT_SECRET) in {}",
                cfg.env_file().display()
            );
        };
        Ok(Api {
            agent,
            token,
            tag: cfg.tag.clone(),
        })
    }

    fn get(&self, path: &str) -> Result<Value> {
        self.agent
            .get(&format!("{API}{path}"))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json")
            .call()
            .map_err(|e| http_err(e).context(format!("GET {path}")))?
            .into_json()
            .context("reading API response")
    }

    /// A single-use, pre-approved key that joins a device with our tag.
    pub fn create_auth_key(&self, name: &str) -> Result<String> {
        let res: Value = self
            .agent
            .post(&format!("{API}/tailnet/-/keys"))
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(json!({
                "capabilities": {"devices": {"create": {
                    "reusable": false, "ephemeral": false, "preauthorized": true, "tags": [self.tag],
                }}},
                "expirySeconds": 900,
                "description": format!("exitnodule {name}"),
            }))
            .map_err(|e| http_err(e).context("creating auth key"))?
            .into_json()?;
        Ok(res["key"]
            .as_str()
            .context("no key in response")?
            .to_string())
    }

    pub fn check_devices(&self) -> Result<()> {
        self.get("/tailnet/-/devices").map(|_| ())
    }

    pub fn delete_devices(&self, hostname: &str) -> Result<usize> {
        let devs = self.get("/tailnet/-/devices")?;
        let mut n = 0;
        for d in devs["devices"].as_array().into_iter().flatten() {
            let short = d["name"]
                .as_str()
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("");
            if d["hostname"].as_str() == Some(hostname) || short == hostname {
                let id = d["id"].as_str().context("device without id")?;
                self.agent
                    .delete(&format!("{API}/device/{id}"))
                    .set("Authorization", &format!("Bearer {}", self.token))
                    .call()
                    .map_err(|e| http_err(e).context("deleting device"))?;
                n += 1;
            }
        }
        Ok(n)
    }

    pub fn policy(&self) -> Result<Value> {
        self.get("/tailnet/-/acl")
    }
}

fn http_err(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            anyhow!(
                "Tailscale API: HTTP {code} {}",
                body.chars().take(300).collect::<String>()
            )
        }
        other => anyhow!("Tailscale API: {other}"),
    }
}
