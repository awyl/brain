//! Second brain server: one binary that serves the chat PWA and drives an agent CLI over a vault.
//!
//!   brain init <dir>   create a vault from the embedded template
//!   brain serve        serve the PWA (env: VAULT_DIR, HOST=127.0.0.1, PORT=8787)
//!   brain ask <msg>    run the agent once (for cron), print the reply
//!
//! AGENT_CMD picks the LLM. It gets the prompt on stdin and runs inside the vault.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, thread};

use tiny_http::{Header, Method, Response, Server};

// Isolated from the host user's Claude settings, hooks and MCP servers; only the vault's own config applies.
const DEFAULT_AGENT_CMD: &str = "claude -p --setting-sources project --strict-mcp-config \
    --permission-mode acceptEdits --allowedTools=Bash(git:*),Bash(grep:*),Bash(ls:*),Bash(date:*)";

const TEMPLATE: &[(&str, &str)] = &[
    ("AGENTS.md", include_str!("../template/AGENTS.md")),
    ("CLAUDE.md", include_str!("../template/CLAUDE.md")),
    (".gitignore", include_str!("../template/.gitignore")),
    (".claude/commands/tidy.md", include_str!("../template/.claude/commands/tidy.md")),
    ("me/preferences.md", include_str!("../template/me/preferences.md")),
    ("me/favorites.md", include_str!("../template/me/favorites.md")),
    ("me/interests.md", include_str!("../template/me/interests.md")),
    ("inbox/.gitkeep", ""),
    ("notes/.gitkeep", ""),
];

const WEB: &[(&str, &str, &str)] = &[
    ("/", "text/html; charset=utf-8", include_str!("../web/index.html")),
    ("/manifest.webmanifest", "application/manifest+json", include_str!("../web/manifest.webmanifest")),
    ("/icon.svg", "image/svg+xml", include_str!("../web/icon.svg")),
];

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["init", dir] => init(Path::new(dir)),
        ["serve"] => serve(),
        ["ask", msg] => ask(&vault_dir(), msg).map(|r| println!("{r}")),
        _ => Err("usage: brain init <dir> | brain serve | brain ask <message>".into()),
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn vault_dir() -> PathBuf {
    env::var_os("VAULT_DIR").map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("error: set VAULT_DIR");
        std::process::exit(1);
    })
}

fn init(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Err(format!("already exists: {}", dir.display()).into());
    }
    for (rel, content) in TEMPLATE {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, content)?;
    }
    git(dir, &["init", "-q"])?;
    git(dir, &["add", "-A"])?;
    git(dir, &["commit", "-qm", "init vault"])?;
    println!("vault created at {}", dir.display());
    Ok(())
}

fn serve() -> Result<()> {
    let vault = vault_dir();
    let host = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = env::var("PORT").unwrap_or_else(|_| "8787".into());
    let server = Server::http(format!("{host}:{port}")).map_err(|e| e.to_string())?;
    println!("serving on http://{host}:{port}, vault {}", vault.display());
    for req in server.incoming_requests() {
        let vault = vault.clone();
        // Agent runs take a while; don't block other requests (the vault lock serializes the work).
        thread::spawn(move || {
            if let Err(e) = handle(req, &vault) {
                eprintln!("request error: {e}");
            }
        });
    }
    Ok(())
}

fn handle(mut req: tiny_http::Request, vault: &Path) -> Result<()> {
    let ctype = |v: &str| Header::from_bytes("Content-Type", v).unwrap();
    match (req.method(), req.url()) {
        (Method::Get, url) => match WEB.iter().find(|(p, ..)| *p == url || (*p == "/" && url == "/index.html")) {
            Some((_, mime, body)) => req.respond(Response::from_string(*body).with_header(ctype(mime)))?,
            None => req.respond(Response::empty(404))?,
        },
        (Method::Post, "/api/chat") => {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            let msg: serde_json::Value = serde_json::from_str(&body)?;
            let msg = msg["message"].as_str().unwrap_or("").trim();
            if msg.is_empty() {
                return Ok(req.respond(Response::empty(400))?);
            }
            let reply = match msg.strip_prefix('+') {
                Some(text) => capture(vault, text.trim()),
                None => ask(vault, msg),
            }
            .unwrap_or_else(|e| format!("Error: {e}"));
            let json = serde_json::json!({ "reply": reply }).to_string();
            req.respond(Response::from_string(json).with_header(ctype("application/json")))?
        }
        _ => req.respond(Response::empty(404))?,
    }
    Ok(())
}

/// Exclusive lock on the vault, shared across processes (serve and cron `ask`).
fn lock(vault: &Path) -> Result<File> {
    let f = File::create(vault.join(".git/brain.lock"))?;
    f.lock()?;
    Ok(f)
}

/// Fast path: write raw text to inbox/ and commit, no LLM.
fn capture(vault: &Path, text: &str) -> Result<String> {
    let mut slug = String::new();
    for c in text.to_lowercase().chars().take(40) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = match slug.trim_matches('-') {
        "" => "note",
        s => s,
    };
    let name = format!("{}-{slug}.md", chrono::Local::now().format("%Y-%m-%d-%H%M%S"));
    let _lock = lock(vault)?;
    fs::write(vault.join("inbox").join(&name), format!("{text}\n"))?;
    git(vault, &["add", &format!("inbox/{name}")])?;
    git(vault, &["commit", "-qm", &format!("inbox: {slug}")])?;
    Ok(format!("Saved to inbox/{name}"))
}

/// Run AGENT_CMD in the vault with the prompt on stdin, then commit anything it left behind.
fn ask(vault: &Path, prompt: &str) -> Result<String> {
    let cmd = env::var("AGENT_CMD").unwrap_or_else(|_| DEFAULT_AGENT_CMD.into());
    let mut parts = cmd.split_whitespace();
    let program = parts.next().ok_or("AGENT_CMD is empty")?;

    let _lock = lock(vault)?;
    let mut child = Command::new(program)
        .args(parts)
        .current_dir(vault)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(prompt.as_bytes())?;
    let out = child.wait_with_output()?;

    // Safety net: never leave agent edits uncommitted.
    git(vault, &["add", "-A"])?;
    if !Command::new("git").args(["diff", "--cached", "--quiet"]).current_dir(vault).status()?.success() {
        git(vault, &["commit", "-qm", "auto: uncommitted agent changes"])?;
    }

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Ok(format!("Agent failed ({}):\n{}", out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn git(dir: &Path, args: &[&str]) -> io::Result<()> {
    let st = Command::new("git").args(args).current_dir(dir).status()?;
    if st.success() { Ok(()) } else { Err(io::Error::other(format!("git {} failed", args.join(" ")))) }
}
