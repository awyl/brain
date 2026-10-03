"""Second brain server: serves the PWA and bridges chat messages to agent.sh.

Env: VAULT_DIR (required), HOST (default 127.0.0.1), PORT (default 8787).
"""
import fcntl
import functools
import json
import os
import re
import subprocess
from datetime import datetime
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent
VAULT = Path(os.environ["VAULT_DIR"]).resolve()
HOST = os.environ.get("HOST", "127.0.0.1")
PORT = int(os.environ.get("PORT", "8787"))


def capture(text):
    """Fast path: write raw text to inbox/ and commit, no LLM."""
    slug = re.sub(r"[^a-z0-9]+", "-", text.lower())[:40].strip("-") or "note"
    path = VAULT / "inbox" / f"{datetime.now():%Y-%m-%d-%H%M%S}-{slug}.md"
    with open(VAULT / ".git" / "brain.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)  # same lock as agent.sh
        path.write_text(text + "\n")
        subprocess.run(["git", "add", path], cwd=VAULT, check=True)
        subprocess.run(["git", "commit", "-qm", f"inbox: {slug}"], cwd=VAULT, check=True)
    return f"Saved to inbox/{path.name}"


def ask(text):
    r = subprocess.run([ROOT / "agent.sh", text], capture_output=True, text=True, timeout=600)
    return r.stdout.strip() if r.returncode == 0 else f"Agent failed ({r.returncode}):\n{r.stderr.strip()}"


class Handler(SimpleHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/api/chat":
            return self.send_error(404)
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        msg = body.get("message", "").strip()
        if not msg:
            return self.send_error(400, "empty message")
        try:
            reply = capture(msg[1:].strip()) if msg.startswith("+") else ask(msg)
        except Exception as e:
            reply = f"Error: {e}"
        data = json.dumps({"reply": reply}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


if __name__ == "__main__":
    handler = functools.partial(Handler, directory=ROOT / "web")
    print(f"serving on http://{HOST}:{PORT}, vault {VAULT}")
    ThreadingHTTPServer((HOST, PORT), handler).serve_forever()
