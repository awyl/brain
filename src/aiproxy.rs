//! The connection to aiproxy (https://github.com/awyl/aiproxy), which holds the provider keys: chat for the agent,
//! embeddings for search, MCP tools.
//!
//!   AIPROXY_URL     default http://127.0.0.1:8080
//!   AIPROXY_TOKEN   its bearer token, if it has one

use std::env;

use serde_json::Value;

use crate::Result;

pub struct Proxy {
    pub url: String,
    pub token: Option<String>,
}

impl Proxy {
    pub fn from_env() -> Self {
        Proxy {
            url: nonempty("AIPROXY_URL").unwrap_or_else(|| "http://127.0.0.1:8080".into()).trim_end_matches('/').into(),
            token: nonempty("AIPROXY_TOKEN"),
        }
    }

    /// POST JSON to `path`; a non-2xx status is an error carrying aiproxy's message.
    pub fn post(&self, path: &str, body: &Value, headers: &[(&str, &str)]) -> Result<ureq::Body> {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut req = agent.post(format!("{}{path}", self.url)).header("Content-Type", "application/json");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.send(body.to_string())?;
        let status = resp.status();
        let mut body = resp.into_body();
        if !status.is_success() {
            let text = body.read_to_string().unwrap_or_default();
            return Err(format!("aiproxy {path}: {status}: {}", text.trim()).into());
        }
        Ok(body)
    }
}

pub fn nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}
