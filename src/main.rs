//! Second brain server: one binary that serves the chat PWA and drives an agent CLI over a vault.
//!
//!   brain init <dir>   create a vault from the embedded template
//!   brain serve        serve the PWA (env: VAULT_DIR, HOST=127.0.0.1, PORT=8787)
//!   brain tunnel       serve + a public Cloudflare tunnel (downloads cloudflared if needed):
//!                      named tunnel if TUNNEL_TOKEN is set (TUNNEL_HOSTNAME for the printed URL), else a quick tunnel
//!   brain ask <msg>    run the agent once (for cron), print the reply
//!   brain search <q>   hybrid full-text + semantic search, top matches with snippets
//!   brain lint         check vault structure against _templates/ and _meta/tags.md (no LLM)
//!   brain upgrade      update an existing vault's managed files from the embedded template
//!
//! AGENT_CMD picks the LLM. It gets the prompt on stdin and runs inside the vault. Its stdout is streamed to
//! the page: parsed as Claude stream-json (AGENT_FORMAT=claude, the default when AGENT_CMD is unset) or as plain text.
//! BRAIN_PUSH=<remote> pushes the vault after every change. TIDY_AT=HH:MM runs the Tidy workflow daily (serve/tunnel).
//! /api/chat requires the token from BRAIN_TOKEN or $VAULT_DIR/.git/brain-token (generated on first serve).

mod lint;
mod search;

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, thread};

use serde_json::{Value, json};
use tiny_http::{Header, Method, Response, Server};

// Isolated from the host user's Claude settings, hooks and MCP servers; only the vault's own config applies.
// An argument list, not a string, because `Bash(brain lint:*)` contains a space.
const DEFAULT_AGENT_CMD: &[&str] = &[
    "claude", "-p", "--setting-sources", "project", "--strict-mcp-config", "--permission-mode", "acceptEdits",
    "--allowedTools=Bash(git:*),Bash(grep:*),Bash(ls:*),Bash(date:*),Bash(brain lint:*),Bash(brain search:*)",
    "--output-format", "stream-json", "--verbose", "--include-partial-messages",
];

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
    ("_templates/place.md", include_str!("../template/_templates/place.md")),
    ("_templates/media.md", include_str!("../template/_templates/media.md")),
    ("_templates/product.md", include_str!("../template/_templates/product.md")),
    ("_templates/person.md", include_str!("../template/_templates/person.md")),
    ("_templates/recipe.md", include_str!("../template/_templates/recipe.md")),
    ("_templates/topic.md", include_str!("../template/_templates/topic.md")),
    ("_meta/tags.md", include_str!("../template/_meta/tags.md")),
    ("_meta/rules.md", include_str!("../template/_meta/rules.md")),
    ("inbox/.gitkeep", ""),
    ("notes/.gitkeep", ""),
];

/// Template files owned by brain: `brain upgrade` overwrites them. Everything else is only added if missing.
const MANAGED: &[&str] = &["AGENTS.md", "CLAUDE.md", ".claude/commands/tidy.md"];

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
        ["lint"] => lint_cmd(&vault_dir()),
        ["search", q] => search_cmd(&vault_dir(), q).map(|r| println!("{r}")),
        ["upgrade"] => upgrade(&vault_dir()),
        _ => Err("usage: brain init <dir> | serve | tunnel | ask <message> | search <query> | lint | upgrade".into()),
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

/// Search results as plain text: one `[[note]] title: snippet` line per hit.
fn search_cmd(vault: &Path, query: &str) -> Result<String> {
    let hits = search::search(vault, query, 8)?;
    if hits.is_empty() {
        return Ok(format!("No matches for {query:?}."));
    }
    Ok(hits
        .iter()
        .map(|h| {
            let name = Path::new(&h.rel).file_stem().unwrap_or_default().to_string_lossy();
            let dir = if h.rel.starts_with("notes/") { String::new() } else { format!("{} ", h.rel) };
            format!("[[{name}]] {dir}{}: {}  ({})", h.title, h.snippet, h.how)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn lint_cmd(vault: &Path) -> Result<()> {
    let issues = lint::Vault::load(vault).check_all();
    for i in &issues {
        println!("{i}");
    }
    let errors = issues.iter().filter(|i| !i.warn).count();
    println!("lint: {errors} problem(s), {} warning(s)", issues.len() - errors);
    if errors > 0 { Err("lint failed".into()) } else { Ok(()) }
}

fn upgrade(vault: &Path) -> Result<()> {
    let _lock = lock(vault)?;
    let mut written = vec![];
    for (rel, content) in TEMPLATE {
        let path = vault.join(rel);
        let current = fs::read_to_string(&path).ok();
        let action = match current {
            None => "added",
            Some(c) if MANAGED.contains(rel) && c != *content => "updated",
            _ => continue,
        };
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, content)?;
        println!("{action} {rel}");
        written.push(*rel);
    }
    if written.is_empty() {
        println!("already up to date");
        return Ok(());
    }
    // Only what upgrade wrote: the owner's own uncommitted edits stay out of this commit.
    git(vault, &[&["add", "--"], written.as_slice()].concat())?;
    git(vault, &[&["commit", "-qm", "upgrade: vault template", "--"], written.as_slice()].concat())?;
    push(vault);
    println!("committed. To convert existing notes, send: restructure the vault");
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
    if search::semantic_enabled() {
        let vault = cfg.vault.clone();
        // Download the model and index the vault up front, so the first search doesn't wait.
        thread::spawn(move || match search::update_index(&vault) {
            Ok((v, n)) => eprintln!("search index: {} files ({n} embedded)", v.len()),
            Err(e) => eprintln!("search index unavailable (full-text only): {e}"),
        });
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
    let bin = cache_dir()?.join("cloudflared");
    if bin.exists() {
        return Ok(bin);
    }
    let arch = match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => "amd64",
        ("linux", "aarch64") => "arm64",
        (os, arch) => return Err(format!("no cloudflared download for {os}/{arch}; install it on PATH").into()),
    };
    download(&format!("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-{arch}"), &bin)?;
    fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(bin)
}

/// $XDG_CACHE_HOME/brain or ~/.cache/brain: downloaded cloudflared and embedding models.
fn cache_dir() -> Result<PathBuf> {
    Ok(env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or("set HOME or XDG_CACHE_HOME")?
        .join("brain"))
}

/// curl `url` to `dest` atomically (via a .part file), so an interrupted download is never mistaken for a good one.
fn download(url: &str, dest: &Path) -> Result<()> {
    eprintln!("downloading {url}");
    fs::create_dir_all(dest.parent().unwrap())?;
    let tmp = dest.with_extension("part");
    if !Command::new("curl").args(["-fsSL", "-o"]).arg(&tmp).arg(url).status()?.success() {
        return Err(format!("download failed: {url}").into());
    }
    fs::rename(&tmp, dest)?;
    Ok(())
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
            let reply = match (msg.strip_prefix('+'), msg.strip_prefix('?')) {
                (Some(text), _) => capture(&cfg.vault, text.trim()),
                (_, Some(q)) => search_cmd(&cfg.vault, q.trim()),
                _ => ask(&cfg.vault, &with_history(&msg, &body["history"]), &mut |ev| send(match ev {
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

/// Run the agent, check what it changed, give it one round to fix problems, then commit and push.
fn ask(vault: &Path, prompt: &str, on: &mut dyn FnMut(Event)) -> Result<String> {
    let _lock = lock(vault)?;
    let before = git_out(vault, &["rev-parse", "HEAD"])?;
    let mut reply = match run_agent(vault, prompt, on) {
        Ok(r) => r,
        Err(e) => e.to_string(),
    };

    let problems = check_changed(vault, &before)?;
    if !problems.is_empty() {
        on(Event::Status(format!("lint: {} problem(s), fixing", problems.len())));
        let fix = format!(
            "The vault checker found problems in files you just changed. Fix exactly these per AGENTS.md, then commit. \
             Reply in one line.\n\n{}",
            problems.join("\n")
        );
        // Only tool steps from the fix round reach the page; the user's reply stays the original one.
        if let Err(e) = run_agent(vault, &fix, &mut |ev| if let Event::Status(_) = ev { on(ev) }) {
            eprintln!("lint fix round failed: {e}");
        }
        let left = check_changed(vault, &before)?;
        if !left.is_empty() {
            reply += &format!("\n\n⚠ {} structure problem(s) left:\n{}", left.len(), left.join("\n"));
        }
    }
    // Refresh vectors for what changed, so the duplicate check below sees them too.
    if search::semantic_enabled() {
        if let Err(e) = search::update_index(vault) {
            eprintln!("search index update failed: {e}");
        }
    }
    // Possible duplicates are only flagged, never auto-merged: the owner decides.
    let dups = lint::Vault::load(vault).duplicates(Some(&changed_paths(vault, &before)?));
    if !dups.is_empty() {
        let list: Vec<String> = dups.iter().map(|i| format!("[[{}]] ↔ {}", i.path.trim_start_matches("notes/").trim_end_matches(".md"), i.msg.trim_start_matches("possible duplicate of "))).collect();
        reply += &format!("\n\nPossibly the same thing (say \"merge them\" if so):\n{}", list.join("\n"));
    }

    // Safety net: never leave agent edits uncommitted.
    git(vault, &["add", "-A"])?;
    if !Command::new("git").args(["diff", "--cached", "--quiet"]).current_dir(vault).status()?.success() {
        git(vault, &["commit", "-qm", "auto: uncommitted agent changes"])?;
    }
    push(vault);
    Ok(reply)
}

/// Files changed since `before`: committed, staged, unstaged or new.
fn changed_paths(vault: &Path, before: &str) -> Result<Vec<String>> {
    let changed = git_out(vault, &["diff", "--name-only", before])?;
    let new = git_out(vault, &["ls-files", "--others", "--exclude-standard"])?;
    Ok(changed.lines().chain(new.lines()).map(str::to_string).collect())
}

/// Lint errors (not warnings) in notes changed since `before`.
fn check_changed(vault: &Path, before: &str) -> Result<Vec<String>> {
    let lint = lint::Vault::load(vault);
    let paths = changed_paths(vault, before)?;
    // New or changed notes must be linked from somewhere; old orphans elsewhere don't block.
    let issues = paths.iter().flat_map(|p| lint.check(p)).chain(lint.orphans(&paths, false));
    Ok(issues.filter(|i| !i.warn).map(|i| i.to_string()).collect())
}

/// Run AGENT_CMD in the vault with the prompt on stdin and stream its progress. Caller holds the lock.
fn run_agent(vault: &Path, prompt: &str, on: &mut dyn FnMut(Event)) -> Result<String> {
    let custom = env::var("AGENT_CMD").ok();
    let claude = match env::var("AGENT_FORMAT") {
        Ok(f) => f == "claude",
        Err(_) => custom.is_none(),
    };
    let cmd: Vec<String> = match custom {
        Some(c) => c.split_whitespace().map(str::to_string).collect(),
        None => DEFAULT_AGENT_CMD.iter().map(|s| s.to_string()).collect(),
    };
    let (program, args) = cmd.split_first().ok_or("AGENT_CMD is empty")?;

    // Put this binary on the agent's PATH so it can run `brain lint`.
    let exe_dir = env::current_exe()?.parent().map(Path::to_path_buf).unwrap_or_default();
    let path = env::join_paths(std::iter::once(exe_dir).chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())))?;
    let mut child = Command::new(program)
        .args(args)
        .env("PATH", path)
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
    if status.success() {
        Ok(reply.trim().to_string())
    } else {
        Err(format!("Agent failed ({status}):\n{}", stderr.trim()).into())
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

fn git_out(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(dir).output()?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> io::Result<()> {
    let st = Command::new("git").args(args).current_dir(dir).status()?;
    if st.success() { Ok(()) } else { Err(io::Error::other(format!("git {} failed", args.join(" ")))) }
}
