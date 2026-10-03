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
| `src/main.rs` | The whole server: `init`, `serve`, `ask`. |
| `web/` | Chat UI (installable PWA), compiled into the binary. |
| `template/` | Starting files for a new vault, compiled into the binary. `AGENTS.md` holds the librarian rules. |

## Setup

```sh
cargo build --release                         # → target/release/brain (one file, copy anywhere)
brain init ~/brain                            # once: create the vault repo
VAULT_DIR=~/brain brain serve                 # listens on 127.0.0.1:8787 (HOST/PORT to change)
tailscale serve --bg 8787                     # private HTTPS on your tailnet
```

The server needs `git` and the agent CLI on `PATH`. On iPhone, open the Tailscale URL in Safari, then Share → Add to Home Screen.

Optional nightly inbox tidy (crontab):

```
0 3 * * * VAULT_DIR=$HOME/brain /usr/local/bin/brain ask "Run the Tidy workflow in AGENTS.md." >> $HOME/brain-tidy.log 2>&1
```

## Using it

- `+ anything`: saved raw to `inbox/` instantly, with no LLM call.
- `remember I prefer window seats`: the agent files it and commits.
- `what coffee do I like?`: the agent searches the vault and answers with a source link.

Requests run one at a time per vault, via a lock in `.git/brain.lock` that `serve` and cron `ask` share. Anything the agent leaves uncommitted gets an `auto:` commit.

## Swapping the LLM

Set `AGENT_CMD` to any agent CLI that reads the prompt from stdin and works in the current directory.
The default is Claude Code, isolated from the host user's settings, hooks and MCP servers (see `DEFAULT_AGENT_CMD` in `src/main.rs`).
The rules live in the vault's `AGENTS.md`, which any agent can read. For tools that look for a different rules file, add a pointer to `AGENTS.md`, the way `CLAUDE.md` does.
Note: `AGENT_CMD` is split on whitespace, so it can't contain quoted arguments. Wrap complex commands in a script.

## Security

The server has no auth and binds to localhost. Only expose it through Tailscale (or similar), never directly to the internet.
