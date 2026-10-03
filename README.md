# Second Brain Server

Chat with an LLM from your phone to capture and query a personal markdown vault (Obsidian-compatible, git-tracked).
This repo builds a single binary, `brain`. The vault is a separate git repo it manages.

```
phone (PWA) ──► brain serve ──► $AGENT_CMD in $VAULT_DIR ──► git commit
                    │
                    └─ "+ text" ──► inbox/<timestamp>.md (no LLM)
```

| Path | What |
|---|---|
| `src/main.rs` | The whole server: `init`, `serve`, `tunnel`, `ask`, `lint`, `upgrade`. |
| `src/lint.rs` | Vault structure checks (no LLM). |
| `web/` | Chat UI (installable PWA), compiled into the binary. Chat history stays on the device; **New** clears it. |
| `template/` | Starting files for a new vault, compiled into the binary. `AGENTS.md` holds the librarian rules, `_templates/` the note types. |

## Setup

```sh
cargo build --release                         # → target/release/brain (one file, copy anywhere)
# fully static, runs on any x86_64 Linux: cargo build --release --target x86_64-unknown-linux-musl
brain init ~/brain                            # once: create the vault repo
VAULT_DIR=~/brain brain serve                 # listens on 127.0.0.1:8787 (HOST/PORT to change)
VAULT_DIR=~/brain brain tunnel                # same, plus a public https://*.trycloudflare.com URL
```

`serve` and `tunnel` print the URL with the access token, e.g. `https://xyz.trycloudflare.com/?token=…`.
Open that on the phone in Safari, then Share → Add to Home Screen. The page saves the token on the device and asks for it if it's missing.

- `tunnel` uses `cloudflared` from `PATH`, or downloads the official release once to `~/.cache/brain/cloudflared` (Linux amd64/arm64). `cloudflared` is stopped when `brain` exits.
- The quick tunnel URL is random and **changes on every restart**, so re-open the new URL after a restart. For a stable URL, use a named tunnel.

### Named tunnel (stable URL, needs a Cloudflare account + domain)

1. Cloudflare dashboard → Zero Trust → Networks → Tunnels → Create tunnel (cloudflared). Copy the tunnel token.
2. Add a public hostname, e.g. `brain.example.com` → service `http://localhost:8787` (match `PORT`).
3. Run:
   ```sh
   TUNNEL_TOKEN=… TUNNEL_HOSTNAME=brain.example.com VAULT_DIR=~/brain brain tunnel
   ```
   `TUNNEL_TOKEN` switches `brain tunnel` to named mode. `TUNNEL_HOSTNAME` is only used to print the URL. Keep both in `.env`, which is gitignored.
- The server needs `git` and the agent CLI on `PATH`.

### Options (env)

| Var | Effect |
|---|---|
| `VAULT_DIR` | Vault path (required). |
| `HOST` / `PORT` | Bind address, default `127.0.0.1:8787`. |
| `BRAIN_TOKEN` | Access token; default is generated into `$VAULT_DIR/.git/brain-token`. |
| `BRAIN_PUSH` | Git remote to push the vault to after every change, e.g. `origin`. Failures are logged, never fatal. |
| `TIDY_AT` | `HH:MM` local time to run the Tidy workflow daily (`serve`/`tunnel`). Skipped when the inbox is empty. |
| `AGENT_CMD` / `AGENT_FORMAT` | Agent CLI and how its output is read, see below. |
| `TUNNEL_TOKEN` / `TUNNEL_HOSTNAME` | Named tunnel, see below. |

Backup setup, once: create a private repo, then `git -C ~/brain remote add origin <url>` and set `BRAIN_PUSH=origin`.

## Using it

- `+ anything`: saved raw to `inbox/` instantly, with no LLM call.
- `remember I prefer window seats`: the agent files it and commits.
- `what coffee do I like?`: the agent searches the vault and answers with a source link.
- `tidy the inbox`: runs the Tidy workflow now.

Replies stream in live: tool steps (`› Edit me/favorites.md`) and text as they happen, then the final answer.
The page sends the last 6 turns along with each message, so follow-ups ("and what about…") work. The chat survives reloads; tap **New** to start fresh.

Requests run one at a time per vault, via a lock in `.git/brain.lock` that `serve` and cron `ask` share. Anything the agent leaves uncommitted gets an `auto:` commit.

## Vault structure

```
inbox/  me/  notes/  _templates/  _meta/tags.md  _meta/rules.md
```

- Every note in `notes/` has a `type` (`place`, `media`, `product`, `person`, `recipe`, `topic`). Its frontmatter fields come from `_templates/<type>.md`; a `# a | b` comment there lists the allowed values.
- Tags must be listed in `_meta/tags.md`. Facts are dated bullets (`- 2026-10-03 · …`). `me/*.md` are hubs linking to notes.
- The vault is its own schema: edit `_templates/` and `_meta/tags.md` to add types, fields or tags. Put your own agent rules in `_meta/rules.md`.

`brain lint` checks every note against that schema: fields, allowed values, tags, `[[links]]`, filenames, and orphan notes (warnings). It exits 1 on errors.
After each agent run, `brain` checks the notes that run changed. On problems, the agent gets one round to fix them; anything left is listed under the reply.

`brain upgrade` updates an existing vault from the binary's template. It overwrites `AGENTS.md`, `CLAUDE.md` and `.claude/commands/tidy.md`, adds missing files, and never touches anything else. Then send `restructure the vault` to convert old notes.

## Swapping the LLM

Set `AGENT_CMD` to any agent CLI that reads the prompt from stdin and works in the current directory.
Its stdout is streamed to the page line by line, and the whole output is the reply.
The default is Claude Code, isolated from the host user's settings, hooks and MCP servers, with `stream-json` output that `brain` parses (see `DEFAULT_AGENT_CMD` in `src/main.rs`).
`AGENT_FORMAT=claude|text` overrides how output is read; it defaults to `claude` only when `AGENT_CMD` is unset.
The rules live in the vault's `AGENTS.md`, which any agent can read. For tools that look for a different rules file, add a pointer to `AGENTS.md`, the way `CLAUDE.md` does.
Note: `AGENT_CMD` is split on whitespace, so it can't contain quoted arguments. Wrap complex commands in a script.

## Security

`/api/chat` requires `Authorization: Bearer <token>`. The token comes from `BRAIN_TOKEN`, else is generated once into `$VAULT_DIR/.git/brain-token` (mode 600, never committed). Anyone with the token can read and write the vault and run the agent, so treat the `?token=` URL like a password. To rotate it, delete the file and restart.

See [BACKLOG.md](BACKLOG.md) for what's next.
