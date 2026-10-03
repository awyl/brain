//! Second brain server: one binary that serves the chat PWA and drives an agent CLI over a vault.
//!
//!   brain init <dir>   create a vault from the embedded template
//!   brain serve        serve the PWA (env: VAULT_DIR, HOST=127.0.0.1, PORT=8787)
//!   brain tunnel       serve + a public Cloudflare tunnel (downloads cloudflared if needed):
//!                      named tunnel if TUNNEL_TOKEN is set (TUNNEL_HOSTNAME for the printed URL), else a quick tunnel
//!   brain ask <msg>    run the agent once (for cron), print the reply
//!
//! AGENT_CMD picks the LLM. It gets the prompt on stdin and runs inside the vault. Its stdout is streamed to
//! the page: parsed as Claude stream-json (AGENT_FORMAT=claude, the default when AGENT_CMD is unset) or as plain text.
//! BRAIN_PUSH=<remote> pushes the vault after every change. TIDY_AT=HH:MM runs the Tidy workflow daily (serve/tunnel).
//! /api/chat requires the token from BRAIN_TOKEN or $VAULT_DIR/.git/brain-token (generated on first serve).

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, thread};

use serde_json::{Value, json};
use tiny_http::{Header, Method, Response, Server};

// Isolated from the host user's Claude settings, hooks and MCP servers; only the vault's own config applies.
const DEFAULT_AGENT_CMD: &str = "claude -p --setting-sources project --strict-mcp-config \
    --permission-mode acceptEdits --allowedTools=Bash(git:*),Bash(grep:*),Bash(ls:*),Bash(date:*) \
    --output-format stream-json --verbose --include-partial-messages";

const TIDY_PROMPT: &str = "Run the Tidy workflow in AGENTS.md.";
/// Follow-up context: how many recent turns the page may send, and how much of each is kept.
const HISTORY_TURNS: usize = 6;
const HISTORY_CHARS: usize = 1500;

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

const WEB: &[(&str, &str, &[u8])] = &[
    ("/", "text/html; charset=utf-8", include_bytes!("../web/index.html")),
    ("/manifest.webmanifest", "application/manifest+json", include_bytes!("../web/manifest.webmanifest")),
    ("/icon.svg", "image/svg+xml", include_bytes!("../web/icon.svg")),
    ("/apple-touch-icon.png", "image/png", include_bytes!("../web/apple-touch-icon.png")),
];

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["init", dir] => init(Path::new(dir)),
        ["serve"] => serve(),
        ["tunnel"] => tunnel(),
        ["ask", msg] => ask(&vault_dir(), msg, &mut |_| {}).map(|r| println!("{r}")),
        _ => Err("usage: brain init <dir> | brain serve | brain tunnel | brain ask <message>".into()),
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

struct Config {
    vault: PathBuf,
    host: String,
    port: String,
    token: String,
    tidy_at: Option<chrono::NaiveTime>,
}

impl Config {
    fn from_env() -> Result<Self> {
        let vault = vault_dir();
        let token = token(&vault)?;
        let tidy_at = match env::var("TIDY_AT") {
            Ok(t) => Some(chrono::NaiveTime::parse_from_str(&t, "%H:%M").map_err(|_| format!("TIDY_AT must be HH:MM, got {t:?}"))?),
            Err(_) => None,
        };
        Ok(Config {
            tidy_at,
            host: env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: env::var("PORT").unwrap_or_else(|_| "8787".into()),
            vault,
            token,
        })
    }
}

/// BRAIN_TOKEN, else a random token persisted in .git/ so it survives restarts and is never committed.
fn token(vault: &Path) -> Result<String> {
    if let Ok(t) = env::var("BRAIN_TOKEN") {
        return Ok(t);
    }
    let path = vault.join(".git/brain-token");
    if let Ok(t) = fs::read_to_string(&path) {
        return Ok(t.trim().to_string());
    }
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    fs::write(&path, &t)?;
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(t)
}

fn serve() -> Result<()> {
    let cfg = Config::from_env()?;
    let server = bind(&cfg)?;
    run(server, cfg);
    Ok(())
}

fn bind(cfg: &Config) -> Result<Server> {
    let server = Server::http(format!("{}:{}", cfg.host, cfg.port)).map_err(|e| e.to_string())?;
    println!("serving on http://{}:{}/?token={}  (vault {})", cfg.host, cfg.port, cfg.token, cfg.vault.display());
    Ok(server)
}

fn run(server: Server, cfg: Config) {
    if let Some(at) = cfg.tidy_at {
        let vault = cfg.vault.clone();
        println!("tidy scheduled daily at {at}");
        thread::spawn(move || tidy_daily(&vault, at));
    }
    let cfg = std::sync::Arc::new(cfg);
    for req in server.incoming_requests() {
        let cfg = cfg.clone();
        // Agent runs take a while; don't block other requests (the vault lock serializes the work).
        thread::spawn(move || {
            if let Err(e) = handle(req, &cfg) {
                eprintln!("request error: {e}");
            }
        });
    }
}

fn tidy_daily(vault: &Path, at: chrono::NaiveTime) {
    loop {
        let now = chrono::Local::now().naive_local();
        let mut next = now.date().and_time(at);
        if next <= now {
            next += chrono::Duration::days(1);
        }
        thread::sleep((next - now).to_std().unwrap_or_default());
        // Skip the LLM call when there's nothing to sort.
        let pending = fs::read_dir(vault.join("inbox"))
            .map(|d| d.flatten().filter(|e| e.file_name() != ".gitkeep").count())
            .unwrap_or(0);
        if pending == 0 {
            eprintln!("tidy: inbox empty, skipped");
            continue;
        }
        eprintln!("tidy: {pending} inbox item(s)");
        match ask(vault, TIDY_PROMPT, &mut |_| {}) {
            Ok(reply) => eprintln!("tidy: done\n{reply}"),
            Err(e) => eprintln!("tidy: error: {e}"),
        }
    }
}

fn tunnel() -> Result<()> {
    let cfg = Config::from_env()?;
    let target = match cfg.host.as_str() {
        "0.0.0.0" => format!("http://127.0.0.1:{}", cfg.port),
        h => format!("http://{h}:{}", cfg.port),
    };
    let token = cfg.token.clone();
    let server = bind(&cfg)?;
    thread::spawn(move || run(server, cfg));

    // Named tunnel: cloudflared reads TUNNEL_TOKEN from the env itself (keeps it out of `ps`);
    // its hostname → service route lives in the Cloudflare dashboard.
    let named = env::var("TUNNEL_TOKEN").is_ok_and(|t| !t.is_empty());
    let mut cmd = Command::new(cloudflared()?);
    match named {
        true => cmd.args(["tunnel", "--no-autoupdate", "run"]),
        false => cmd.args(["tunnel", "--no-autoupdate", "--url", &target]),
    };
    cmd.stderr(Stdio::piped());
    // Kill cloudflared if brain dies, however it dies (SIGTERM, SIGKILL, crash).
    #[cfg(target_os = "linux")]
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    // cloudflared logs to stderr; pick out the public URL, keep the rest for when it fails.
    let mut log = Vec::new();
    let mut connected = false;
    for line in BufReader::new(child.stderr.take().unwrap()).lines() {
        let line = line?;
        if named && !connected && line.contains("Registered tunnel connection") {
            connected = true;
            match env::var("TUNNEL_HOSTNAME") {
                Ok(host) => println!("public:  https://{host}/?token={token}"),
                Err(_) => println!("tunnel connected; route your hostname to {target} in the Cloudflare dashboard"),
            }
        } else if let Some(url) = line.split_whitespace().find(|w| w.starts_with("https://") && w.ends_with(".trycloudflare.com")) {
            println!("public:  {url}/?token={token}");
        }
        log.push(line);
    }
    let status = child.wait()?;
    let tail = log[log.len().saturating_sub(10)..].join("\n");
    Err(format!("cloudflared exited ({status}):\n{tail}").into())
}

/// cloudflared from PATH, else a cached download of the official release (Linux only).
fn cloudflared() -> Result<PathBuf> {
    if Command::new("cloudflared").arg("--version").output().is_ok_and(|o| o.status.success()) {
        return Ok("cloudflared".into());
    }
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or("set HOME or XDG_CACHE_HOME")?
        .join("brain");
    let bin = cache.join("cloudflared");
    if bin.exists() {
        return Ok(bin);
    }
    let arch = match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => "amd64",
        ("linux", "aarch64") => "arm64",
        (os, arch) => return Err(format!("no cloudflared download for {os}/{arch}; install it on PATH").into()),
    };
    let url = format!("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-{arch}");
    println!("downloading {url}");
    fs::create_dir_all(&cache)?;
    let tmp = cache.join("cloudflared.part");
    if !Command::new("curl").args(["-fsSL", "-o"]).arg(&tmp).arg(&url).status()?.success() {
        return Err("cloudflared download failed".into());
    }
    fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    fs::rename(&tmp, &bin)?;
    Ok(bin)
}

fn handle(mut req: tiny_http::Request, cfg: &Config) -> Result<()> {
    let ctype = |v: &str| Header::from_bytes("Content-Type", v).unwrap();
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    match (req.method(), path.as_str()) {
        (Method::Get, path) => {
            eprintln!("GET {path}{}", if req.url().contains("token=") { " (with ?token=)" } else { "" });
            match WEB.iter().find(|(p, ..)| *p == path || (*p == "/" && path == "/index.html")) {
                Some((_, mime, body)) => req.respond(Response::from_data(*body).with_header(ctype(mime)))?,
                None => req.respond(Response::empty(404))?,
            }
        }
        (Method::Post, "/api/chat") => {
            let auth = req.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.as_str());
            if !auth.and_then(|a| a.strip_prefix("Bearer ")).is_some_and(|t| same(t, &cfg.token)) {
                let why = match auth {
                    None => "no Authorization header".to_string(),
                    Some(a) => format!("wrong token (got {} chars, want Bearer + {})", a.len(), cfg.token.len()),
                };
                eprintln!("POST /api/chat 401: {why}");
                return Ok(req.respond(Response::empty(401))?);
            }
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            let body: Value = serde_json::from_str(&body)?;
            let msg = body["message"].as_str().unwrap_or("").trim().to_string();
            if msg.is_empty() {
                return Ok(req.respond(Response::empty(400))?);
            }
            eprintln!("POST /api/chat 200");
            // Server-sent events over a hand-written chunked response, flushed per event so
            // progress reaches the phone live. Write errors (phone gone) are ignored: the agent finishes anyway.
            let mut w = req.into_writer();
            w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n")?;
            let mut send = |ev: Value| {
                let data = format!("data: {ev}\n\n");
                let _ = write!(w, "{:x}\r\n{data}\r\n", data.len()).and_then(|_| w.flush());
            };
            let reply = match msg.strip_prefix('+') {
                Some(text) => capture(&cfg.vault, text.trim()),
                None => ask(&cfg.vault, &with_history(&msg, &body["history"]), &mut |ev| send(match ev {
                    Event::Delta(t) => json!({ "type": "delta", "text": t }),
                    Event::Status(t) => json!({ "type": "status", "text": t }),
                })),
            }
            .unwrap_or_else(|e| format!("Error: {e}"));
            send(json!({ "type": "done", "reply": reply }));
            let _ = w.write_all(b"0\r\n\r\n").and_then(|_| w.flush());
        }
        _ => req.respond(Response::empty(404))?,
    }
    Ok(())
}

/// Constant-time string comparison for the token.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
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
    push(vault);
    Ok(format!("Saved to inbox/{name}"))
}

/// Progress from a running agent.
enum Event {
    /// A piece of reply text.
    Delta(String),
    /// A tool step, e.g. "Edit me/favorites.md".
    Status(String),
}

/// Prefix the message with recent turns from the page, so follow-ups ("and what about…") work with any agent.
fn with_history(msg: &str, history: &Value) -> String {
    let turns = history.as_array().map(Vec::as_slice).unwrap_or_default();
    let turns = &turns[turns.len().saturating_sub(HISTORY_TURNS)..];
    if turns.is_empty() {
        return msg.to_string();
    }
    let mut p = String::from("Recent conversation, for context only:\n");
    for t in turns {
        let who = if t["role"] == "assistant" { "Assistant" } else { "User" };
        let text: String = t["text"].as_str().unwrap_or("").chars().take(HISTORY_CHARS).collect();
        p += &format!("{who}: {text}\n");
    }
    p + "\nCurrent message:\n" + msg
}

/// Run AGENT_CMD in the vault with the prompt on stdin, stream its progress, then commit and push.
fn ask(vault: &Path, prompt: &str, on: &mut dyn FnMut(Event)) -> Result<String> {
    let custom = env::var("AGENT_CMD").ok();
    let claude = match env::var("AGENT_FORMAT") {
        Ok(f) => f == "claude",
        Err(_) => custom.is_none(),
    };
    let cmd = custom.unwrap_or_else(|| DEFAULT_AGENT_CMD.into());
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
    let stderr = child.stderr.take().unwrap();
    let stderr = thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        s
    });

    let mut reply = String::new();
    let mut said_something = false;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line?;
        if !claude {
            on(Event::Delta(format!("{line}\n")));
            reply += &line;
            reply.push('\n');
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v["type"].as_str() {
            Some("stream_event") => match v["event"]["type"].as_str() {
                Some("message_start") if said_something => on(Event::Delta("\n\n".into())),
                Some("content_block_delta") if v["event"]["delta"]["type"] == "text_delta" => {
                    said_something = true;
                    on(Event::Delta(v["event"]["delta"]["text"].as_str().unwrap_or("").into()));
                }
                _ => {}
            },
            Some("assistant") => {
                for c in v["message"]["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "tool_use") {
                    on(Event::Status(tool_summary(c, vault)));
                }
            }
            Some("result") => reply = v["result"].as_str().unwrap_or("").to_string(),
            _ => {}
        }
    }
    let status = child.wait()?;
    let stderr = stderr.join().unwrap_or_default();

    // Safety net: never leave agent edits uncommitted.
    git(vault, &["add", "-A"])?;
    if !Command::new("git").args(["diff", "--cached", "--quiet"]).current_dir(vault).status()?.success() {
        git(vault, &["commit", "-qm", "auto: uncommitted agent changes"])?;
    }
    push(vault);

    if status.success() {
        Ok(reply.trim().to_string())
    } else {
        Ok(format!("Agent failed ({status}):\n{}", stderr.trim()))
    }
}

/// "Edit me/favorites.md", "Bash git commit -m …": tool name plus its most telling argument.
fn tool_summary(tool: &Value, vault: &Path) -> String {
    let input = &tool["input"];
    let arg = ["file_path", "command", "pattern", "path"].iter().find_map(|k| input[k].as_str()).unwrap_or("");
    let prefix = format!("{}/", vault.display());
    let arg = arg.strip_prefix(&prefix).unwrap_or(arg).lines().next().unwrap_or("");
    let arg: String = arg.chars().take(80).collect();
    format!("{} {arg}", tool["name"].as_str().unwrap_or("tool")).trim_end().to_string()
}

/// BRAIN_PUSH=<remote>: back up the vault after every change. Failure is logged, never fatal.
fn push(vault: &Path) {
    let Ok(remote) = env::var("BRAIN_PUSH") else { return };
    let ok = Command::new("git")
        .args(["push", "-q", &remote, "HEAD"])
        .current_dir(vault)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("push to {remote} failed");
    }
}

fn git(dir: &Path, args: &[&str]) -> io::Result<()> {
    let st = Command::new("git").args(args).current_dir(dir).status()?;
    if st.success() { Ok(()) } else { Err(io::Error::other(format!("git {} failed", args.join(" ")))) }
}
