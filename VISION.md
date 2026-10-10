# Vision

**Goal: a second brain that records me, learns what I learn, and becomes similar to me.**
Not a map of the world's knowledge, but a map of *me*: what I did, liked, thought and learned, and why. It should be able to answer "what's my…", "did I…", "what do I know about…", and eventually "would I like…?".

## Principles

1. **Record me, including what I know.** Knowledge I learned goes in, in my words, dated, with where I learned it. General knowledge I never learned stays out: the LLM already has it at question time, and storing it drowns my own facts and mixes unchecked claims into my records.
2. **Provenance is sacred.** My facts are dated bullets in my words. Anything the LLM adds (summaries, context, synthesis) is marked as such and links to the notes it comes from. Never mix the two.
3. **Why beats what.** A list of things I like is shallow. My reasons ("light roast, quiet, solo seating") are what make the brain similar to me.
4. **Structure is enforced by code, not by prompts.** Templates are the schema; `brain lint` checks it; the agent gets a fix round. Prompts drift; checks don't.
5. **Grow the graph where it earns its place.** A page exists when something recurs or matters, not because an entity was mentioned once.
6. **Plain files, replaceable parts.** Markdown + git is the source of truth. The LLM, the front end and the tunnel can all be swapped.

## Ideas from LLM wikis

LLM wikis ingest automatically: they extract every entity and concept, give each a page, link everything and fill pages from the LLM's own knowledge.

| Idea | Verdict | Why |
|---|---|---|
| Entity pages, cross-linked | **Take**, for recurring entities only | Real graph to browse (`[[susanna-clarke]]` links her books), without hundreds of stubs. |
| Automatic ingestion of sources | **Take**, for links I share | A summary plus *my* reaction, linked into the graph. |
| Concept pages filled by the LLM | **Skip** | Breaks principles 1 and 2. At most a one-line `(context)` marker, never mixed with my facts. |

## Roadmap

In order. Details in [BACKLOG.md](BACKLOG.md).

1. **Profile synthesis**: the core of "similar to me".
2. **Recurring-entity pages**
3. **Capture the why**
4. **Link ingestion**
