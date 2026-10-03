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
_meta/        rules.md (owner's extra rules)
```

## Notes

- Every note in `notes/` has a `type` matching a file in `_templates/`. To create one, copy that template and fill it in. If no specific type fits, use `topic`.
- Filenames: lowercase-kebab-case, e.g. `notes/ichiran-namba.md`. Link with `[[ichiran-namba]]`.
- Frontmatter is flat `key: value` lines. Lists only as `[a, b]`. No nesting, no multi-line values.
- Keep every field from the template; leave the value empty if unknown. Where the template shows `# a | b | c`, the value must be one of those. Don't copy the `#` comment into notes.
- `aliases`: other names someone might use for the same thing: short name, full name, original title, common misspelling. Search them before creating a note.
- `tags`: lowercase, words joined with `-`, singular (`bread`, `street-food`). Run `brain tags` and reuse an existing tag before inventing a new one. A few broad, reusable tags beat many one-off ones; specifics belong in the text and `aliases`. Don't repeat the `type` as a tag.
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
1. Run `brain search "<name and a few key words>"` to find an existing note: it matches names, aliases, content and meaning ("Piranesi" finds a note titled "The House of Halls"). Update it, or create a new one from the matching template. Never create a second note for the same thing.
2. Link every new note from somewhere: the right `me/*.md` hub (favorites, preferences, interests) and/or a related note. Unlinked notes get lost. Don't ask, just link.
3. Set `updated:` and `Related:` links.
4. Commit.

### Query — "what's my …", "did I …", "find …"
1. Search the vault first: `brain search "<question in your own words>"` for fuzzy recall, then grep for exact facts and fields. Never answer personal facts from memory or guesswork.
2. Answer briefly and cite the source note(s) as `[[note-name]]`.
3. If nothing is found, say so and offer to capture it.

### Tidy — sort the inbox
For each file in `inbox/` (skip `.gitkeep`), oldest first:
1. Turn it into notes per the rules above (update existing or create from templates), linked from hubs where relevant.
2. Delete the inbox file with `git rm` (history keeps the original).
3. If an item is ambiguous, leave it in `inbox/` and list it in your summary.
Then run `brain lint`. For each "possible duplicate" that is clearly the same thing, merge (see Merge). For each "similar tags" warning, pick one spelling and rewrite it in every note that uses the other. Commit and report: what went where, what was left, which duplicates you merged or skipped.

### Restructure — bring the vault in line with these rules
For every file in `notes/` and `me/`: set `type` and all template fields, turn facts into dated bullets (use the file's git history for dates; else today), move entities out of `me/*.md` into their own notes, fix tags and links. Then run `brain lint`, fix every error, and merge clear duplicates. Commit, then report what changed.

### Merge — "merge X and Y", or a clear duplicate
1. Keep the older note (by git history). Move the other's facts into it (keep dates), union `tags` and `aliases`, and add the other's name to `aliases`.
2. Replace every `[[other]]` link in the vault with `[[kept]]`.
3. `git rm` the other note. Commit as `merge: other into kept`.

## Checks

`brain search "<query>"` ranks notes by words and meaning, with a snippet each. `brain tags` lists tags in use. `brain lint` checks the whole vault: frontmatter fields and allowed values, tag format, links, filenames, orphans, possible duplicates, similar tags. Run it whenever you want.
When you finish, `brain` also checks the files you changed. If anything is wrong you'll get one follow-up message listing the problems. Fix exactly those, then commit.

## Git

- Commit after every change set with `git add -A` then `git commit -m "<verb>: <what>"`, e.g. `capture: coffee preference`, `tidy: 4 inbox items`.
- Never rewrite history or force-push.
