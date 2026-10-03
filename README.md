# Second Brain Server

Chat with an LLM from your phone to capture and query a personal markdown vault (Obsidian-compatible, git-tracked).
This repo is the server. The vault is a separate git repo it manages.

```
phone (PWA) ──► server.py ──► agent.sh ──► $AGENT_CMD in $VAULT_DIR ──► git commit
                  │
                  └─ "+ text" ──► inbox/<timestamp>.md (no LLM)
```

| Path | What |
|---|---|
| `server.py` | Serves the PWA and `POST /api/chat`. Python stdlib only. |
| `agent.sh` | Runs the agent in the vault with a lock. The only LLM-specific file. |
| `web/` | Chat UI (installable PWA). |
| `template/` | Starting files for a new vault. `AGENTS.md` holds the librarian rules. |
| `init-vault.sh` | Creates a vault from `template/`. |

## Setup

```sh
./init-vault.sh ~/brain                 # once
VAULT_DIR=~/brain python3 server.py     # listens on 127.0.0.1:8787
tailscale serve --bg 8787               # private HTTPS on your tailnet
```

On iPhone, open the Tailscale URL in Safari, then Share → Add to Home Screen.

Optional nightly inbox tidy (crontab):

```
0 3 * * * VAULT_DIR=$HOME/brain /path/to/agent.sh "Run the Tidy workflow in AGENTS.md." >> $HOME/brain-tidy.log 2>&1
```

## Using it

- `+ anything`: saved raw to `inbox/` instantly, with no LLM call.
- `remember I prefer window seats`: the agent files it and commits.
- `what coffee do I like?`: the agent searches the vault and answers with a source link.

## Swapping the LLM

Set `AGENT_CMD` to any agent CLI that reads the prompt from stdin and works in the current directory.
The rules live in the vault's `AGENTS.md`, which any agent can read. For tools that look for a different rules file, add a pointer to `AGENTS.md`, the way `CLAUDE.md` does.
Note: `AGENT_CMD` is split on spaces, so it can't contain quoted arguments. Wrap complex commands in a script.

## Security

The server has no auth and binds to localhost. Only expose it through Tailscale (or similar), never directly to the internet.
