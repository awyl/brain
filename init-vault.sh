#!/bin/sh
# Usage: init-vault.sh <dir>  — create a new vault repo from template/.
set -eu
dir="${1:?usage: init-vault.sh <dir>}"
[ -e "$dir" ] && { echo "already exists: $dir" >&2; exit 1; }
cp -r "$(dirname "$0")/template" "$dir"
cd "$dir"
git init -q
git add -A
git commit -qm "init vault"
echo "vault created at $dir"
