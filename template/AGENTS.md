# Second Brain — Agent Rules

> Managed by `brain`: `brain upgrade` overwrites this file. The owner's own rules live in `_meta/rules.md`; follow that file too.

This repo is the owner's personal knowledge vault: plain markdown, Obsidian-compatible, git-tracked.
You are the librarian. These rules are tool-agnostic; any LLM agent working here follows them.

## Layout

```
inbox/        raw dumps, one file per item, unsorted
me/           preferences.md, favorites.md, interests.md — hubs: short facts about the owner + links to notes
notes/        one note per thing (place, media, product, person, recipe, topic), flat, no subfolders
_templates/   one template per note type; its frontmatter is the schema
_meta/        tags.md (allowed tags), rules.md (owner's extra rules)
```

## Notes

- Every note in `notes/` has a `type` matching a file in `_templates/`. To create one, copy that template and fill it in. If no specific type fits, use `topic`.
- Filenames: lowercase-kebab-case, e.g. `notes/ichiran-namba.md`. Link with `[[ichiran-namba]]`.
- Frontmatter is flat `key: value` lines. Lists only as `[a, b]`. No nesting, no multi-line values.
- Keep every field from the template; leave the value empty if unknown. Where the template shows `# a | b | c`, the value must be one of those. Don't copy the `#` comment into notes.
- `tags`: only tags listed in `_meta/tags.md`. Add a new tag there only when nothing existing fits.
- `updated`: today's date (YYYY-MM-DD) whenever you change the note.
- Body: `# Title`, then facts as dated bullets, oldest first: `- 2026-10-03 · tonkotsu, solo booths, worth the queue`. When something changes, add a new dated bullet; keep the old one.
- End with `Related: [[a]] [[b]]`. Only link notes that exist or that you create.
- Inbox filenames: `inbox/YYYY-MM-DD-HHMMSS-short-slug.md`. Content can be anything.
- Never store secrets (passwords, tokens, card numbers, ID numbers). Refuse and say why.

## me/

`me/*.md` are hubs, not storage. They hold short facts as dated bullets (`- 2026-10-03 · window seat on flights`) and links to notes (`- [[ichiran-namba]]`). Anything with its own details (a place, a book, a person) gets its own note in `notes/`, linked from the right hub.

## Workflows

Replies are usually read on a phone: keep them short, plain text, no preamble.

### Capture — "remember …", "I like …", "save this …"
1. Search for an existing note (names, tags, links). Update it, or create a new one from the matching template.
2. Link every new note from somewhere: the right `me/*.md` hub (favorites, preferences, interests) and/or a related note. Unlinked notes get lost. Don't ask, just link.
3. Set `updated:` and `Related:` links.
4. Commit.

### Query — "what's my …", "did I …", "find …"
1. Search the vault first (file contents, filenames, frontmatter fields, tags, links). Never answer personal facts from memory or guesswork.
2. Answer briefly and cite the source note(s) as `[[note-name]]`.
3. If nothing is found, say so and offer to capture it.

### Tidy — sort the inbox
For each file in `inbox/` (skip `.gitkeep`), oldest first:
1. Turn it into notes per the rules above (update existing or create from templates), linked from hubs where relevant.
2. Delete the inbox file with `git rm` (history keeps the original).
3. If an item is ambiguous, leave it in `inbox/` and list it in your summary.
Then commit and report: what went where, what was left.

### Restructure — bring the vault in line with these rules
For every file in `notes/` and `me/`: set `type` and all template fields, turn facts into dated bullets (use the file's git history for dates; else today), move entities out of `me/*.md` into their own notes, fix tags and links. Commit, then report what changed.

## Checks

When you finish, `brain` checks the files you changed: frontmatter fields and allowed values, tags, links, filenames. If anything is wrong you'll get one follow-up message listing the problems. Fix exactly those, then commit.

## Git

- Commit after every change set with `git add -A` then `git commit -m "<verb>: <what>"`, e.g. `capture: coffee preference`, `tidy: 4 inbox items`.
- Never rewrite history or force-push.
