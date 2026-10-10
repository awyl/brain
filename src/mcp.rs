//! `brain mcp`: the brain as an MCP server (stdio, JSON-RPC 2.0, one message per line), so agents in
//! other projects can consult the owner's profile and preferences, and report new ones.
//!
//! Read access never includes notes tagged `private`. Writes only ever go to inbox/ (Tidy files them later).

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::{Result, capture, search, search_cmd};

/// Sent at `initialize`. The first line must stand alone: pi lists only that line in its system prompt.
/// Clients that drop server instructions still see the record rules in brain_remember's description.
const INSTRUCTIONS: &str = "The user's second brain: check it before choices that depend on their taste, and record their \
preferences, what they did and what they learned with brain_remember, without asking.

Read it before decisions that depend on their taste or way of working (languages, tools, architecture, style, \
recommendations): brain_profile once per task, then brain_search for specifics. If it conflicts with the conversation, \
the conversation wins.

Record without being asked, with brain_remember, one fact per call, in their words, with the why when they give one \
and the project name as source:
- a lasting preference, or a correction of your choice (\"make it a single binary\");
- something they did: started, finished, shipped, decided (\"started Hades\", \"moved the blog to Zola\");
- something they learned or now know (\"btrfs snapshots need a separate subvolume for /home\").
Skip task details, temporary state, secrets, and general knowledge they didn't learn. Don't ask first: it goes to \
their inbox for review.";

pub fn serve(vault: &Path) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            reply(&mut stdout, json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }))?;
            continue;
        };
        // Notifications (no id) get no response.
        let Some(id) = msg.get("id").cloned() else { continue };
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "brain", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => Ok(call(vault, &msg["params"]["name"], &msg["params"]["arguments"])),
            other => Err(json!({ "code": -32601, "message": format!("method not found: {other}") })),
        };
        let response = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
        };
        reply(&mut stdout, response)?;
    }
    Ok(())
}

fn reply(out: &mut impl Write, v: Value) -> io::Result<()> {
    writeln!(out, "{v}")?;
    out.flush()
}

fn tools() -> Value {
    let string = |desc: &str| json!({ "type": "string", "description": desc });
    json!([
        {
            "name": "brain_profile",
            "description": "The user's profile, synthesized from their notes: work & tools (languages, style, what they push back on), \
                taste, habits, interests, dislikes. Check it before choosing languages, tools, architecture or style for them.",
            "inputSchema": { "type": "object", "properties": {
                "section": string("Optional section name, e.g. \"Work & tools\" or \"Taste\". Omit for the whole profile."),
            }},
        },
        {
            "name": "brain_search",
            "description": "Search the user's notes by words and meaning. Returns the best matching notes with a snippet each.",
            "inputSchema": { "type": "object", "properties": { "query": string("What to look for, in natural language.") }, "required": ["query"] },
        },
        {
            "name": "brain_note",
            "description": "Read one of the user's notes in full, by name (as returned by brain_search, e.g. \"fuglen-tokyo\" or \"work\").",
            "inputSchema": { "type": "object", "properties": { "name": string("Note name, without .md.") }, "required": ["name"] },
        },
        {
            "name": "brain_remember",
            "description": "Save something lasting about the user to their inbox, for them to review. Call it without being asked when \
                they state a preference or correct your choice, did something (started, finished, shipped, decided), or learned \
                something. One fact per call, in their words, with the why when given. Not for task details, temporary state, \
                secrets, or general knowledge they didn't learn.",
            "inputSchema": { "type": "object", "properties": {
                "text": string("The fact, in the user's words."),
                "source": string("Where it came from, e.g. the project name."),
            }, "required": ["text"] },
        },
    ])
}

fn call(vault: &Path, name: &Value, args: &Value) -> Value {
    let arg = |k: &str| args[k].as_str().unwrap_or("").trim().to_string();
    let result = match name.as_str().unwrap_or("") {
        "brain_profile" => profile(vault, &arg("section")),
        "brain_search" => match arg("query") {
            q if q.is_empty() => Err("query is required".into()),
            q => search_cmd(vault, &q, false),
        },
        "brain_note" => note(vault, &arg("name")),
        "brain_remember" => match arg("text") {
            t if t.is_empty() => Err("text is required".into()),
            t => {
                let source = arg("source");
                let from = if source.is_empty() { "an agent".to_string() } else { format!("an agent in {source}") };
                capture(vault, &format!("{t}\n\n(via MCP, from {from})")).map(|saved| format!("{saved}. The user's librarian will file it."))
            }
        },
        other => Err(format!("unknown tool: {other}").into()),
    };
    match result {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(e) => json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
    }
}

/// me/profile.md without frontmatter, optionally just one `## Section` (case-insensitive substring match).
fn profile(vault: &Path, section: &str) -> Result<String> {
    let text = fs::read_to_string(vault.join("me/profile.md")).map_err(|_| "no profile yet")?;
    let body = search::split_frontmatter(&text).1.trim();
    if section.is_empty() {
        return Ok(body.to_string());
    }
    let wanted = section.to_lowercase();
    let mut parts = body.split("\n## ").skip(1);
    let found = parts.find(|p| p.lines().next().unwrap_or("").to_lowercase().contains(&wanted));
    match found {
        Some(p) => Ok(format!("## {}", p.trim())),
        None => {
            let names: Vec<&str> = body.split("\n## ").skip(1).filter_map(|p| p.lines().next()).collect();
            Err(format!("no section matching {section:?}; sections: {}", names.join(", ")).into())
        }
    }
}

/// A note or hub by name. Private notes are refused.
fn note(vault: &Path, name: &str) -> Result<String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err("give a note name like \"fuglen-tokyo\"".into());
    }
    let name = name.trim_end_matches(".md");
    let path = ["notes", "me"].iter().map(|d| vault.join(d).join(format!("{name}.md"))).find(|p| p.exists());
    let text = fs::read_to_string(path.ok_or_else(|| format!("no note named {name:?}; try brain_search"))?)?;
    if search::is_private(&text) {
        return Err(format!("{name} is private").into());
    }
    Ok(text)
}
