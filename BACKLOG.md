# Backlog

- [ ] **Run as a service**: systemd unit for `brain tunnel` (restart on failure, `EnvironmentFile=.env`), plus a named tunnel for a stable URL. Today the server only lives as long as whoever started it.
- [ ] **Turn on backup**: create a private repo, `git -C <vault> remote add origin <url>`, set `BRAIN_PUSH=origin`.
- [ ] **Share-sheet capture**: iOS Shortcut "Save to brain" that posts `+ <shared text/link>`. Needs a plain JSON endpoint (e.g. `POST /api/capture`), since Shortcuts handles a single JSON reply better than an event stream.
