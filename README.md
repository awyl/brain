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
| `src/search.rs` | Hybrid search: BM25 full-text + local embeddings. |
| `src/fetch.rs` | `brain fetch`: web page → plain text, fenced as untrusted. |
| `web/` | Chat UI (installable PWA), compiled into the binary. Chat history stays on the device; **New** clears it. |
| `template/` | Starting files for a new vault, compiled into the binary. `AGENTS.md` holds the librarian rules, `_templates/` the note types. |

## Setup

```sh
cargo build --release                         # → target/release/brain (one file, copy anywhere)
# fully static, runs on any x86_64 Linux (~9MB): cargo build --release --target x86_64-unknown-linux-musl
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
| `TIDY_AT` | `HH:MM` local time for the daily job (`serve`/`tunnel`): Tidy if the inbox has items, then Profile if notes changed since the last one. |
| `AGENT_CMD` / `AGENT_FORMAT` | Agent CLI and how its output is read, see below. |
| `TUNNEL_TOKEN` / `TUNNEL_HOSTNAME` | Named tunnel, see below. |

Backup setup, once: create a private repo, then `git -C ~/brain remote add origin <url>` and set `BRAIN_PUSH=origin`.

## Using it

- `+ anything`: saved raw to `inbox/` instantly, with no LLM call.
- `? anything`: search the vault directly, with no LLM call. Top notes with a snippet each.
- `remember I prefer window seats`: the agent files it and commits.
- `what coffee do I like?`: the agent searches the vault and answers with a source link.
- `tidy the inbox`: runs the Tidy workflow now.

Replies stream in live: tool steps (`› Edit me/favorites.md`) and text as they happen, then the final answer.
The page sends the last 6 turns along with each message, so follow-ups ("and what about…") work. The chat survives reloads; tap **New** to start fresh.

Requests run one at a time per vault, via a lock in `.git/brain.lock` that `serve` and cron `ask` share. Anything the agent leaves uncommitted gets an `auto:` commit.

## Vault structure

```
inbox/  me/  notes/  _templates/  _meta/rules.md
```

- Every note in `notes/` has a `type` (`place`, `media`, `product`, `person`, `recipe`, `entity`, `source`, `topic`). Its frontmatter fields come from `_templates/<type>.md`; a `# a | b` comment there lists the allowed values.
- Tags are free-form, lowercase-kebab (`street-food`); `brain tags` lists them with counts. Facts are dated bullets (`- 2026-10-03 · …`). `me/*.md` are hubs linking to notes.
- The vault is its own schema: edit `_templates/` to add types or fields. Put your own agent rules in `_meta/rules.md`.

- `aliases: [..]` (Obsidian's own field) holds other names for the same thing, so the agent finds an existing note instead of creating a second one.

`brain lint` checks every note against that schema: fields, allowed values, tag format, `[[links]]`, filenames, plus warnings for orphan notes, similar tags (`bread`/`breads`, `coffee`/`cofee`) and possible duplicates (same type, plus a shared name/alias/title, one name inside the other, or a near-typo). It exits 1 on errors.
The agent may run `brain lint` itself. Tidy and Restructure use it to find duplicates, and merge the clear ones (`merge X and Y` works in chat too).
After each agent run, `brain` checks the notes that run changed. On problems, the agent gets one round to fix them; anything left is listed under the reply. Possible duplicates are listed too, but never merged without asking.

`brain upgrade` updates an existing vault from the binary's template. It overwrites `AGENTS.md`, `CLAUDE.md` and `.claude/commands/tidy.md`, adds missing files, and never touches anything else. Then send `restructure the vault` to convert old notes.

## The brain

What makes it more than a notebook (see [VISION.md](VISION.md)):
- **Profile:** `me/profile.md` is a generated synthesis of who you are (taste, habits, interests, dislikes, open questions). Every claim links its evidence. It's rebuilt by the daily job when notes changed, or on "update my profile". The agent reads it for "would I like…" questions.
- **Entities:** creators, cities, cuisines and brands that appear in 2 or more notes get an `entity` page linking them. Lint warns about recurring names without one; Tidy creates them.
- **Why:** when a capture has no reason or feeling, the agent asks one short follow-up and records the answer.
- **Links:** shared URLs are fetched (`brain fetch`). An article becomes a `source` note with an LLM summary and your take; a reference page becomes a linked fact on the thing it's about.
- **Provenance:** your facts are dated bullets in your words. Anything the LLM writes sits under a `(LLM)` heading or in a generated file.

Fetched pages are untrusted input to an agent that can edit the vault. The output is fenced as data and `AGENTS.md` says to ignore instructions in it, but that's mitigation, not a guarantee. The agent can only edit the vault and run git, so the damage is limited to reversible commits.

## Search

`brain search "cozy place for a flat white"` combines two rankings:
- **Full text (BM25):** words in the body, frontmatter values, titles and aliases (counted 3×). Pure Rust, no index.
- **Meaning:** a local embedding model, downloaded on first use to `~/.cache/brain/models/`. The vectors live in `$VAULT_DIR/.git/brain-index.json` (never committed) and get updated incrementally after every agent run. `serve` builds the index at startup.

The two lists are merged by reciprocal rank fusion. Each hit shows why it matched (`text 7.0, meaning 0.65`).
The agent uses `brain search` before creating a note (to avoid duplicates) and to answer fuzzy questions.

| Var | Effect |
|---|---|
| `BRAIN_EMBED` | `english` (default, `bge-small-en-v1.5`, ~130MB) or `multilingual` (`multilingual-e5-small`, ~470MB). |
| `BRAIN_SEMANTIC=0` | Full-text only: no model download, no index. |
| `BRAIN_MIN_SIM` / `BRAIN_DUP_SIM` | Tuning: the semantic match floor (0.58), and the content similarity that flags a duplicate (0.90). |

Small embedding models can't tell "the same book" from "two books on the same topic" by content alone. That's why content duplicates are only flagged for near-copies, and the main defense is searching before creating.

## Swapping the LLM

Set `AGENT_CMD` to any agent CLI that reads the prompt from stdin and works in the current directory.
Its stdout is streamed to the page line by line, and the whole output is the reply.
The default is Claude Code, isolated from the host user's settings, hooks and MCP servers, with `stream-json` output that `brain` parses (see `DEFAULT_AGENT_CMD` in `src/main.rs`).
`AGENT_FORMAT=claude|text` overrides how output is read; it defaults to `claude` only when `AGENT_CMD` is unset.
The rules live in the vault's `AGENTS.md`, which any agent can read. For tools that look for a different rules file, add a pointer to `AGENTS.md`, the way `CLAUDE.md` does.
Note: `AGENT_CMD` is split on whitespace, so it can't contain quoted arguments. Wrap complex commands in a script.

## Security

`/api/chat` requires `Authorization: Bearer <token>`. The token comes from `BRAIN_TOKEN`, else is generated once into `$VAULT_DIR/.git/brain-token` (mode 600, never committed). Anyone with the token can read and write the vault and run the agent, so treat the `?token=` URL like a password. To rotate it, delete the file and restart.

See [VISION.md](VISION.md) for the goal and principles, and [BACKLOG.md](BACKLOG.md) for what's next.
