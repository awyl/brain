#!/bin/sh
# Usage: agent.sh "message"
# Runs the configured agent CLI inside the vault, one at a time. The prompt goes on stdin.
# This is the only place that knows which LLM is used: swap it via AGENT_CMD.
set -euf
: "${VAULT_DIR:?set VAULT_DIR}"
AGENT_CMD="${AGENT_CMD:-claude -p --permission-mode acceptEdits --allowedTools=Bash(git:*),Bash(grep:*),Bash(ls:*),Bash(date:*)}"

cd "$VAULT_DIR"
exec 9>.git/brain.lock
flock 9

printf "%s" "$1" | $AGENT_CMD

# Safety net: never leave agent edits uncommitted.
git add -A
git diff --cached --quiet || git commit -qm "auto: uncommitted agent changes"
