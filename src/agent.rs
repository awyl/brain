//! Built-in agent: an OpenAI chat-completions tool loop over the vault, with LLM calls routed through aiproxy
//! (https://github.com/awyl/aiproxy). Used when AGENT_CMD is unset.
//!
//!   BRAIN_MODEL     model id, required, e.g. `opencode-go/deepseek-v4.1-flash` (must be on the chat-completions surface)
//!   AIPROXY_URL     default http://127.0.0.1:8080
//!   AIPROXY_TOKEN   bearer token for aiproxy, if it has one
//!   BRAIN_MCP       aiproxy MCP servers to offer as tools (its X-MCP-Servers header, e.g. `searxng,ctx7`); unset = none
//!
//! Local tools: read/write/edit files inside the vault (never .git/), and `run` for an allowlist of commands,
//! executed without a shell. This is the agent's whole reach: fetched pages are untrusted input.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::env;

use serde_json::{Value, json};

use crate::{Event, Result};

const MAX_ROUNDS: usize = 40;
/// Tool output beyond this is cut, so one big file or grep can't blow the context.
const MAX_OUTPUT: usize = 30_000;

pub struct Llm {
    pub url: String,
    pub token: Option<String>,
    pub model: String,
    pub mcp: Option<String>,
    /// One id per run, sent as `x-opencode-session`: OpenCode Go requires it, and it keeps the rounds on one prompt cache.
    pub session: String,
}

impl Llm {
    pub fn from_env() -> Result<Self> {
        let nonempty = |k| env::var(k).ok().filter(|v: &String| !v.is_empty());
        Ok(Llm {
            url: nonempty("AIPROXY_URL").unwrap_or_else(|| "http://127.0.0.1:8080".into()).trim_end_matches('/').into(),
            token: nonempty("AIPROXY_TOKEN"),
            model: nonempty("BRAIN_MODEL").ok_or("set BRAIN_MODEL (e.g. opencode-go/deepseek-v4.1-flash), or AGENT_CMD")?,
            mcp: nonempty("BRAIN_MCP"),
            session: format!("brain-{}-{}", std::process::id(), chrono::Local::now().timestamp_millis()),
        })
    }

    fn post(&self, path: &str, body: &Value) -> Result<ureq::Body> {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut req = agent
            .post(format!("{}{path}", self.url))
            .header("Content-Type", "application/json")
            .header("x-opencode-session", &self.session)
            .header("x-opencode-client", "brain");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        if let Some(m) = &self.mcp {
            req = req.header("X-MCP-Servers", m);
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

    fn mcp_call(&self, method: &str, params: Value) -> Result<Value> {
        let v: Value = serde_json::from_reader(self.post("/mcp", &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))?.into_reader())?;
        match v.get("error") {
            Some(e) => Err(format!("mcp {method}: {}", e["message"].as_str().unwrap_or("error")).into()),
            None => Ok(v["result"].clone()),
        }
    }
}

/// Run the agent with `prompt` until it answers without tool calls. Caller holds the vault lock.
pub fn run(vault: &Path, prompt: &str, on: &mut dyn FnMut(Event)) -> Result<String> {
    run_with(&Llm::from_env()?, vault, prompt, on)
}

pub fn run_with(llm: &Llm, vault: &Path, prompt: &str, on: &mut dyn FnMut(Event)) -> Result<String> {
    let mut tools = local_tools();
    if llm.mcp.is_some() {
        // MCP being down shouldn't stop note-taking: run without those tools.
        match llm.mcp_call("tools/list", json!({})) {
            Ok(r) => tools.extend(r["tools"].as_array().into_iter().flatten().map(|t| {
                json!({ "type": "function", "function": { "name": t["name"], "description": t["description"], "parameters": t["inputSchema"] } })
            })),
            Err(e) => eprintln!("agent: MCP tools unavailable: {e}"),
        }
    }
    let mut messages = vec![json!({ "role": "system", "content": system_prompt(vault) }), json!({ "role": "user", "content": prompt })];
    // Text from an earlier round ("Let me check…") is set apart from the next round's text.
    let mut sep = false;
    for _ in 0..MAX_ROUNDS {
        let body = json!({ "model": llm.model, "messages": messages, "tools": tools, "stream": true });
        let (text, calls) = stream(llm.post("/v1/chat/completions", &body)?, &mut |t| {
            if std::mem::take(&mut sep) {
                on(Event::Delta("\n\n".into()));
            }
            on(Event::Delta(t.into()))
        })?;
        if calls.is_empty() {
            return Ok(text.trim().to_string());
        }
        messages.push(json!({ "role": "assistant", "content": text, "tool_calls": calls }));
        for c in &calls {
            let name = c["function"]["name"].as_str().unwrap_or("");
            let args: Value = serde_json::from_str(c["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
            on(Event::Status(summary(name, &args)));
            let out = match local(vault, name, &args) {
                Some(r) => r,
                None => mcp_tool(llm, name, &args),
            }
            .unwrap_or_else(|e| format!("Error: {e}"));
            messages.push(json!({ "role": "tool", "tool_call_id": c["id"], "content": truncate(out) }));
        }
        sep |= !text.is_empty();
    }
    Err(format!("agent stopped after {MAX_ROUNDS} rounds without a final answer").into())
}

/// AGENTS.md is the librarian's rulebook; _meta/rules.md holds the owner's own additions.
fn system_prompt(vault: &Path) -> String {
    let read = |p| fs::read_to_string(vault.join(p)).unwrap_or_default();
    format!(
        "{}\n\n# Owner's rules (_meta/rules.md)\n\n{}\n\n# Tools\n\nYou work in the vault through tools; paths are relative to the vault root. \
         Shell commands go through `run` as an argument list (no shell: no pipes, globs or quotes). Today is {}.",
        read("AGENTS.md"),
        read("_meta/rules.md"),
        chrono::Local::now().format("%Y-%m-%d"),
    )
}

/// Read an OpenAI SSE stream: text deltas go to `on`; tool calls are assembled from their fragments.
fn stream(body: ureq::Body, on: &mut dyn FnMut(&str)) -> Result<(String, Vec<Value>)> {
    let mut text = String::new();
    let mut calls: Vec<Value> = vec![];
    for line in BufReader::new(body.into_reader()).lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(e) = v.get("error") {
            return Err(format!("LLM error: {e}").into());
        }
        let delta = &v["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            text += t;
            on(t);
        }
        for tc in delta["tool_calls"].as_array().into_iter().flatten() {
            let i = tc["index"].as_u64().unwrap_or(calls.len() as u64) as usize;
            if i >= calls.len() {
                calls.resize(i + 1, json!({ "type": "function", "id": "", "function": { "name": "", "arguments": "" } }));
            }
            let c = &mut calls[i];
            if let Some(id) = tc["id"].as_str() {
                c["id"] = id.into();
            }
            for k in ["name", "arguments"] {
                if let Some(s) = tc["function"][k].as_str() {
                    c["function"][k] = format!("{}{s}", c["function"][k].as_str().unwrap_or("")).into();
                }
            }
        }
    }
    Ok((text, calls))
}

fn local_tools() -> Vec<Value> {
    let tool = |name: &str, desc: &str, props: Value, required: &[&str]| {
        json!({ "type": "function", "function": { "name": name, "description": desc,
            "parameters": { "type": "object", "properties": props, "required": required } } })
    };
    let s = json!({ "type": "string" });
    vec![
        tool("read", "Read a file in the vault.", json!({ "path": s }), &["path"]),
        tool("write", "Create or overwrite a file in the vault.", json!({ "path": s, "content": s }), &["path", "content"]),
        tool(
            "edit",
            "Replace `old` with `new` in a vault file. `old` must occur exactly once; include enough context.",
            json!({ "path": s, "old": s, "new": s }),
            &["path", "old", "new"],
        ),
        tool(
            "run",
            "Run a command in the vault root, as an argument list without a shell, e.g. [\"git\", \"commit\", \"-m\", \"capture: coffee\"]. \
             Allowed: git, grep, ls, date, brain lint|search|tags|fetch.",
            json!({ "argv": { "type": "array", "items": s } }),
            &["argv"],
        ),
    ]
}

/// A local tool's result, or None if `name` isn't local (then it's an MCP tool).
fn local(vault: &Path, name: &str, args: &Value) -> Option<Result<String>> {
    ["read", "write", "edit", "run"].contains(&name).then(|| local_tool(vault, name, args))
}

fn local_tool(vault: &Path, name: &str, args: &Value) -> Result<String> {
    let arg = |k: &str| args[k].as_str().ok_or_else(|| format!("missing argument {k:?}"));
    match name {
            "read" => Ok(fs::read_to_string(vault_path(vault, arg("path")?)?)?),
            "write" => {
                let path = vault_path(vault, arg("path")?)?;
                fs::create_dir_all(path.parent().unwrap())?;
                fs::write(&path, arg("content")?)?;
                Ok("ok".into())
            }
            "edit" => {
                let path = vault_path(vault, arg("path")?)?;
                let (old, text) = (arg("old")?, fs::read_to_string(&path)?);
                match text.matches(old).count() {
                    1 => fs::write(&path, text.replacen(old, arg("new")?, 1))?,
                    0 => return Err("`old` not found".into()),
                    n => return Err(format!("`old` occurs {n} times; add context to make it unique").into()),
                }
                Ok("ok".into())
            }
            "run" => {
                let argv: Vec<String> = args["argv"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(String::from)).collect();
                allowed(&argv)?;
                let program = match argv[0].as_str() {
                    "brain" => env::current_exe()?,
                    p => PathBuf::from(p),
                };
                let out = Command::new(program).args(&argv[1..]).current_dir(vault).output()?;
                let mut s = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
                if !out.status.success() {
                    s += &format!("\n[{}]", out.status);
                }
                Ok(s)
            }
            _ => unreachable!(),
        }
}

fn mcp_tool(llm: &Llm, name: &str, args: &Value) -> Result<String> {
    let r = llm.mcp_call("tools/call", json!({ "name": name, "arguments": args }))?;
    let text: Vec<&str> = r["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect();
    let text = text.join("\n");
    if r["isError"] == true { Err(text.into()) } else { Ok(text) }
}

/// A path inside the vault: relative (or absolute under the vault), no `..`, never into .git/ (hooks, config).
fn vault_path(vault: &Path, p: &str) -> Result<PathBuf> {
    let rel = Path::new(p).strip_prefix(vault).unwrap_or(Path::new(p));
    let ok = rel.components().all(|c| matches!(c, Component::Normal(n) if n != ".git") || c == Component::CurDir);
    if !ok || rel.as_os_str().is_empty() {
        return Err(format!("path must be inside the vault and outside .git/: {p}").into());
    }
    Ok(vault.join(rel))
}

/// The command allowlist. git can't set config (aliases and hooks run arbitrary commands);
/// grep and ls can't reach outside the vault.
fn allowed(argv: &[String]) -> Result<()> {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    let ok = match a[..] {
        ["git", ref rest @ ..] => !rest.iter().any(|x| *x == "config" || x.starts_with("-c") || x.starts_with("--config") || x.starts_with("--exec-path")),
        ["grep" | "ls", ref rest @ ..] => !rest.iter().any(|x| x.contains("..") || x.starts_with('/') || x.contains("=/")),
        ["date", ..] => true,
        ["brain", "lint" | "search" | "tags" | "fetch", ..] => true,
        _ => false,
    };
    if ok { Ok(()) } else { Err(format!("not allowed: {}", a.join(" ")).into()) }
}

/// "Edit me/favorites.md", "Run git commit -m …": the step as the page shows it.
fn summary(name: &str, args: &Value) -> String {
    let arg = match &args["argv"] {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
        _ => args["path"].as_str().or(args["query"].as_str()).unwrap_or("").to_string(),
    };
    let arg: String = arg.lines().next().unwrap_or("").chars().take(80).collect();
    let mut name = name.to_string();
    if let Some(f) = name.get_mut(..1) {
        f.make_ascii_uppercase();
    }
    format!("{name} {arg}").trim_end().to_string()
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s += "\n[output truncated]";
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn paths_stay_in_vault() {
        let v = Path::new("/v");
        assert_eq!(vault_path(v, "notes/a.md").unwrap(), Path::new("/v/notes/a.md"));
        assert_eq!(vault_path(v, "/v/notes/a.md").unwrap(), Path::new("/v/notes/a.md"));
        assert_eq!(vault_path(v, "./me/x.md").unwrap(), Path::new("/v/./me/x.md"));
        for bad in ["../x", "/etc/passwd", ".git/hooks/pre-commit", "notes/../../x", ""] {
            assert!(vault_path(v, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn command_allowlist() {
        let ok = |s: &str| allowed(&s.split(' ').map(String::from).collect::<Vec<_>>()).is_ok();
        assert!(ok("git commit -m x") && ok("grep -ri coffee notes") && ok("brain lint") && ok("date"));
        for bad in ["rm -rf .", "git config alias.x !sh", "git -c core.hooksPath=x commit", "grep -r key /etc", "ls ..", "brain serve", "sh -c ls"] {
            assert!(!ok(bad), "{bad}");
        }
    }

    /// Full loop against a fake aiproxy: round 1 calls `write`, round 2 answers.
    #[test]
    fn loop_runs_tools_then_answers() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        thread::spawn(move || {
            let rounds = [
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write","arguments":"{\"path\":\"inbox/t.md\","}}]}}]}"#.to_string()
                    + "\n\n"
                    + r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"content\":\"hi\"}"}}]}}]}"#
                    + "\n\ndata: [DONE]\n\n",
                r#"data: {"choices":[{"delta":{"content":"Saved "}}]}"#.to_string() + "\n\n" + r#"data: {"choices":[{"delta":{"content":"it."}}]}"# + "\n\ndata: [DONE]\n\n",
            ];
            for (mut req, body) in server.incoming_requests().zip(rounds) {
                let mut sent = String::new();
                req.as_reader().read_to_string(&mut sent).unwrap();
                assert!(sent.contains("\"model\":\"m\""));
                req.respond(tiny_http::Response::from_string(body)).unwrap();
            }
        });
        let vault = env::temp_dir().join(format!("brain-agent-test-{}", std::process::id()));
        fs::create_dir_all(&vault).unwrap();
        let llm = Llm { url, token: None, model: "m".into(), mcp: None, session: "s".into() };
        let mut events = vec![];
        let reply = run_with(&llm, &vault, "save hi", &mut |e| events.push(match e {
            Event::Delta(t) | Event::Status(t) => t,
        }))
        .unwrap();
        assert_eq!(reply, "Saved it.");
        assert_eq!(fs::read_to_string(vault.join("inbox/t.md")).unwrap(), "hi");
        assert_eq!(events, ["Write inbox/t.md", "Saved ", "it."]);
        fs::remove_dir_all(&vault).unwrap();
    }
}
