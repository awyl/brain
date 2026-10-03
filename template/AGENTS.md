# Second Brain — Agent Rules

This repo is the owner's personal knowledge vault: plain markdown, Obsidian-compatible, git-tracked.
You are the librarian. These rules are tool-agnostic; any LLM agent working here follows them.

## Layout

```
inbox/   raw dumps, one file per item, unsorted
me/      preferences.md, favorites.md, interests.md — facts about the owner
notes/   one atomic note per topic/thing/place/person (flat, no subfolders)
```

## Conventions

- Filenames: lowercase-kebab-case, e.g. `notes/osaka-ramen.md`.
- Inbox filenames: `inbox/YYYY-MM-DD-HHMMSS-short-slug.md`. Content can be anything: a sentence, a link, a pasted article.
- Every note in `me/` and `notes/` starts with frontmatter:
  ```
  ---
  tags: [food, travel]
  updated: YYYY-MM-DD
  ---
  ```
- Link related notes with `[[note-name]]` (filename without `.md`). Create a link whenever a note mentions another existing note.
- Keep notes short and factual. Use bullets and the owner's own wording where possible.
- When a fact changes, update it in place and keep a one-line history: `- window seat (was aisle until 2026-10)`.
- Never store secrets (passwords, tokens, card numbers, ID numbers). Refuse and say why.

## Workflows

Replies are usually read on a phone: keep them short, plain text, no preamble.

### Capture — "remember …", "I like …", "save this …"
1. If it clearly belongs in an existing note or `me/*.md`, add it there under the right heading.
2. Otherwise create a new note in `notes/`, or, if unsure, write it to `inbox/` per the naming rule.
3. Update `updated:` and add `[[links]]`.
4. Commit.

### Query — "what's my …", "did I …", "find …"
1. Search the vault first (`grep -ri`, filenames, tags, links). Never answer personal facts from memory or guesswork.
2. Answer briefly and cite the source note(s) as `[[note-name]]`.
3. If nothing is found, say so and offer to capture it.

### Tidy — sort the inbox
For each file in `inbox/` (skip `.gitkeep`), oldest first:
1. Extract the facts. Merge into an existing note, `me/*.md`, or a new note in `notes/`.
2. Add tags, `updated:`, and `[[links]]`.
3. Delete the inbox file with `git rm` (history keeps the original).
4. If an item is ambiguous, leave it in `inbox/` and list it in your summary.
Then commit and report: what went where, what was left.

## Git

- Commit after every change set: `git add -A && git commit -m "<verb>: <what>"`, e.g. `capture: coffee preference`, `tidy: 4 inbox items`.
- Never rewrite history or force-push.
