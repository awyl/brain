# Backlog

See [VISION.md](VISION.md) for the goal and principles these serve.

## Brain (toward "similar to me")

- [ ] **1. Profile synthesis**: `me/profile.md`, regenerated on a schedule (e.g. weekly, like `TIDY_AT`) from all notes: taste patterns, likes and dislikes, habits. Every claim links its evidence (`[[fuglen-tokyo]]`), and the file is marked as generated, so it's never mistaken for raw facts. It goes into the agent's context for every question and answers "would I like X?".
- [ ] **2. Recurring-entity pages**: during Tidy, an entity that appears in ≥2 notes (creator, city, cuisine, person) gets its own page (new types like `creator`/`city`, or `topic`) linking those notes. Below the threshold, it stays plain text. Lint could warn about recurring names without a page.
- [ ] **3. Capture the why**: when a capture has no reason or feeling in it, the agent asks one short follow-up ("what did you like about it?") and records the answer as a dated fact. Never more than one question; skippable.
- [ ] **4. Link ingestion**: `+ <url>` gets fetched during Tidy (agent web fetch, or `curl` in `brain`). The result is a short summary note marked as LLM-written, plus my reaction if I gave one, linked to related notes. No full-text copies.

## Infra

- [ ] **Run as a service**: systemd unit for `brain tunnel` (restart on failure, `EnvironmentFile=.env`), plus a named tunnel for a stable URL. Today the server only lives as long as whoever started it.
- [ ] **Turn on backup**: create a private repo, `git -C <vault> remote add origin <url>`, set `BRAIN_PUSH=origin`.
- [ ] **Share-sheet capture**: iOS Shortcut "Save to brain" that posts `+ <shared text/link>`. Needs a plain JSON endpoint (e.g. `POST /api/capture`), since Shortcuts handles a single JSON reply better than an event stream.
