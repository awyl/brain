//! Structural checks on a vault, no LLM. The schema lives in the vault itself:
//! `_templates/<type>.md` frontmatter lists each type's fields (a `# a | b` comment lists allowed values).
//! Tags are free-form but must be lowercase-kebab; near-duplicate tags across the vault are warned about.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;

pub struct Issue {
    pub path: String,
    pub msg: String,
    /// Reported, but doesn't fail `brain lint` or trigger a fix round.
    pub warn: bool,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}: {}", if self.warn { "warning: " } else { "" }, self.path, self.msg)
    }
}

struct Field {
    name: String,
    /// Allowed values (empty = anything).
    allowed: Vec<String>,
    /// `[]` in the template: the value must be a `[a, b]` list.
    list: bool,
}

pub struct Vault<'a> {
    root: &'a Path,
    types: HashMap<String, Vec<Field>>,
    /// Lowercased basenames of every note, for resolving [[links]] the way Obsidian does.
    names: HashSet<String>,
    /// Up-to-date embeddings from the search index (empty if there's none): content-level duplicates.
    vectors: HashMap<String, Vec<f32>>,
    dup_threshold: f32,
}

impl<'a> Vault<'a> {
    pub fn load(root: &'a Path) -> Self {
        let mut types = HashMap::new();
        for rel in md_files(root, "_templates") {
            let text = fs::read_to_string(root.join(&rel)).unwrap_or_default();
            let fields = frontmatter(&text)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    let (value, comment) = split_comment(v);
                    let allowed = match comment {
                        Some(c) if c.contains('|') => c.split('|').map(|s| s.trim().to_string()).collect(),
                        _ => vec![],
                    };
                    Some(Field { name: k.trim().to_string(), allowed, list: value.starts_with('[') })
                })
                .collect();
            types.insert(stem(&rel), fields);
        }
        let names = md_files(root, "").iter().map(|p| stem(p).to_lowercase()).collect();
        let vectors = if crate::search::semantic_enabled() { crate::search::fresh_vectors(root) } else { HashMap::new() };
        Vault { root, types, names, vectors, dup_threshold: crate::search::dup_threshold() }
    }

    /// Every note in notes/ and me/, plus orphan warnings.
    pub fn check_all(&self) -> Vec<Issue> {
        let notes = md_files(self.root, "notes");
        let mut issues: Vec<Issue> = notes.iter().chain(&md_files(self.root, "me")).flat_map(|p| self.check(p)).collect();

        issues.extend(self.orphans(&notes, true));
        issues.extend(self.duplicates(None));
        issues.extend(self.similar_tags());
        issues.extend(self.recurring_without_page());
        issues
    }

    /// Warnings for names used by 2+ notes in an entity-like field (creator, city…) that have no page of their own.
    fn recurring_without_page(&self) -> Vec<Issue> {
        const FIELDS: &[&str] = &["creator", "author", "city", "country", "brand", "cuisine"];
        let mut known = HashSet::new();
        let mut uses: HashMap<String, (String, usize)> = HashMap::new();
        for rel in md_files(self.root, "notes") {
            let text = fs::read_to_string(self.root.join(&rel)).unwrap_or_default();
            let fm: HashMap<&str, &str> = frontmatter(&text)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim(), split_comment(v).0)))
                .collect();
            known.extend(names(&rel, &text, fm.get("aliases").copied()));
            for value in FIELDS.iter().filter_map(|f| fm.get(f)).filter(|v| !v.is_empty()) {
                let shown = value.trim_matches(['[', ']', '"']).to_string();
                uses.entry(name_tokens(value).concat()).or_insert((shown, 0)).1 += 1;
            }
        }
        let mut out: Vec<Issue> = uses
            .into_iter()
            .filter(|(key, (_, n))| *n >= 2 && !key.is_empty() && !known.contains(key))
            .map(|(_, (name, n))| Issue {
                path: "entities".into(),
                msg: format!("recurring '{name}' ({n} notes) has no page"),
                warn: true,
            })
            .collect();
        out.sort_by(|a, b| a.msg.cmp(&b.msg));
        out
    }

    /// Every tag in notes and hubs, with how many files use it, most used first.
    pub fn tag_counts(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for rel in md_files(self.root, "notes").iter().chain(&md_files(self.root, "me")) {
            let text = fs::read_to_string(self.root.join(rel)).unwrap_or_default();
            let tags = frontmatter(&text)
                .unwrap_or_default()
                .into_iter()
                .find_map(|l| l.strip_prefix("tags:").map(|v| split_comment(v).0.to_string()));
            for t in tags.as_deref().and_then(list).unwrap_or_default() {
                *counts.entry(t.to_string()).or_default() += 1;
            }
        }
        let mut counts: Vec<_> = counts.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        counts
    }

    /// Warnings for tags that are probably one tag spelled two ways: plural/singular or a missing letter.
    fn similar_tags(&self) -> Vec<Issue> {
        let counts = self.tag_counts();
        let mut out = vec![];
        for (i, (a, na)) in counts.iter().enumerate() {
            for (b, nb) in &counts[i + 1..] {
                let (sa, sb) = (singular(a), singular(b));
                // A missing/extra letter (coffee/cofee), not a swapped one: class/glass are different words.
                let typo = sa.len().min(sb.len()) >= 4 && sa.len() != sb.len() && levenshtein(&sa, &sb) == 1;
                if sa == sb || typo {
                    out.push(Issue {
                        path: "tags".into(),
                        msg: format!("similar tags: {a} ({na}), {b} ({nb}): pick one"),
                        warn: true,
                    });
                }
            }
        }
        out
    }

    /// Warnings for same-type notes that look like the same thing: a shared name/alias/title, one name
    /// inside the other (`ichiran` / `ichiran-namba`), a near-typo, or near-identical content (embeddings).
    /// With `only`, just pairs touching those paths.
    pub fn duplicates(&self, only: Option<&[String]>) -> Vec<Issue> {
        struct Note {
            rel: String,
            kind: String,
            tokens: Vec<String>,
            keys: HashSet<String>,
            links: HashSet<String>,
        }
        let notes: Vec<Note> = md_files(self.root, "notes")
            .into_iter()
            .filter_map(|rel| {
                let text = fs::read_to_string(self.root.join(&rel)).ok()?;
                let fm: HashMap<&str, &str> = frontmatter(&text)?
                    .into_iter()
                    .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim(), split_comment(v).0)))
                    .collect();
                let tokens = name_tokens(&stem(&rel));
                let keys = names(&rel, &text, fm.get("aliases").copied());
                let links = links(&text).into_iter().map(|l| l.to_lowercase()).collect();
                Some(Note { kind: fm.get("type")?.to_string(), rel, tokens, keys, links })
            })
            .collect();

        // One name inside the other, adding at most one word (`ichiran` / `ichiran-namba`, not `ramen` / `ramen-shops-in-tokyo`).
        let subset = |a: &[String], b: &[String]| {
            a.concat().len() >= 4 && b.len() <= a.len() + 1 && a.iter().all(|t| b.contains(t))
        };
        let mut out = vec![];
        for (i, a) in notes.iter().enumerate() {
            for b in &notes[i + 1..] {
                if a.kind != b.kind || only.is_some_and(|o| !o.contains(&a.rel) && !o.contains(&b.rel)) {
                    continue;
                }
                // Notes that link each other are known to be distinct things (that's how a false alarm is dismissed).
                if a.links.contains(&stem(&b.rel).to_lowercase()) || b.links.contains(&stem(&a.rel).to_lowercase()) {
                    continue;
                }
                let (ka, kb) = (a.tokens.concat(), b.tokens.concat());
                let named = !a.keys.is_disjoint(&b.keys)
                    || subset(&a.tokens, &b.tokens)
                    || subset(&b.tokens, &a.tokens)
                    || (ka.len().min(kb.len()) >= 6 && levenshtein(&ka, &kb) <= 2);
                // Entity pages are mostly link lists, so their content looks alike; compare their names only.
                let content = match (self.vectors.get(&a.rel), self.vectors.get(&b.rel)) {
                    _ if a.kind == "entity" => false,
                    (Some(va), Some(vb)) => crate::search::cosine(va, vb) >= self.dup_threshold,
                    _ => false,
                };
                if named || content {
                    let why = if named { "" } else { " (similar content)" };
                    out.push(Issue { path: a.rel.clone(), msg: format!("possible duplicate of [[{}]]{why}", stem(&b.rel)), warn: true });
                }
            }
        }
        out
    }

    /// Notes among `rels` that no note or hub links to. Only links from notes/ and me/ count
    /// (AGENTS.md and templates contain example links).
    pub fn orphans(&self, rels: &[String], warn: bool) -> Vec<Issue> {
        let mut linked = HashSet::new();
        for rel in md_files(self.root, "notes").iter().chain(&md_files(self.root, "me")) {
            let text = fs::read_to_string(self.root.join(rel)).unwrap_or_default();
            linked.extend(links(&text).into_iter().map(|l| l.to_lowercase()));
        }
        rels.iter()
            .filter(|rel| rel.starts_with("notes/") && self.root.join(rel).exists())
            .filter(|rel| !linked.contains(&stem(rel).to_lowercase()))
            .map(|rel| Issue {
                path: rel.clone(),
                msg: "orphan: link it from a me/ hub or a related note".into(),
                warn,
            })
            .collect()
    }

    /// One note, by vault-relative path. Files outside notes/ and me/ aren't checked.
    pub fn check(&self, rel: &str) -> Vec<Issue> {
        let mut out = vec![];
        let mut err = |msg: String| out.push(Issue { path: rel.to_string(), msg, warn: false });
        let in_notes = rel.starts_with("notes/");
        if !(in_notes || rel.starts_with("me/")) || !rel.ends_with(".md") {
            return vec![];
        }
        if in_notes && !is_kebab(&stem(rel)) {
            err("filename must be lowercase-kebab-case".into());
        }
        let Ok(text) = fs::read_to_string(self.root.join(rel)) else { return out };
        let Some(lines) = frontmatter(&text) else {
            err("missing frontmatter (--- block at the top)".into());
            return out;
        };

        let mut fm = HashMap::new();
        for line in lines {
            match line.split_once(':') {
                Some((k, v)) => {
                    fm.insert(k.trim().to_string(), split_comment(v).0.to_string());
                }
                None => err(format!("frontmatter line is not `key: value`: {line:?}")),
            }
        }

        match fm.get("tags").map(String::as_str) {
            None => err("missing field 'tags'".into()),
            Some(v) => match list(v) {
                None => err(format!("tags must be a [a, b] list, got {v:?}")),
                Some(tags) => {
                    for tag in tags {
                        let normal = normalize_tag(tag);
                        if tag != normal {
                            err(format!("tag '{tag}' should be '{normal}' (lowercase, words joined with -)"));
                        }
                    }
                }
            },
        }
        match fm.get("updated") {
            None => err("missing field 'updated'".into()),
            Some(v) if chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_err() => {
                err(format!("updated must be YYYY-MM-DD, got {v:?}"))
            }
            _ => {}
        }

        if in_notes {
            match fm.get("type").map(String::as_str) {
                None | Some("") => err("missing field 'type'".into()),
                Some(t) => match self.types.get(t) {
                    None => {
                        let mut known: Vec<_> = self.types.keys().map(String::as_str).collect();
                        known.sort();
                        err(format!("unknown type '{t}' (one of: {})", known.join(", ")));
                    }
                    Some(fields) => {
                        for Field { name, allowed, list: is_list } in fields {
                            match fm.get(name) {
                                None => err(format!("missing field '{name}' (type {t}; leave empty if unknown)")),
                                Some(v) if *is_list && list(v).is_none() => {
                                    err(format!("{name} must be a [a, b] list, got {v:?}"))
                                }
                                Some(v) if !v.is_empty() && !allowed.is_empty() && !allowed.contains(v) => {
                                    err(format!("{name}: {v:?} is not one of {}", allowed.join(" | ")))
                                }
                                _ => {}
                            }
                        }
                    }
                },
            }
        }

        for link in links(&text) {
            if !self.names.contains(&link.to_lowercase()) {
                err(format!("broken link [[{link}]]"));
            }
        }
        out
    }
}

/// Frontmatter lines between the leading `---` and the closing `---`.
fn frontmatter(text: &str) -> Option<Vec<&str>> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---").map(|i| i + 1).or_else(|| rest.starts_with("---").then_some(0))?;
    Some(rest[..end].lines().filter(|l| !l.trim().is_empty()).collect())
}

/// `value  # comment` → ("value", Some("comment")). A `#` only starts a comment at the start or after a space.
fn split_comment(v: &str) -> (&str, Option<&str>) {
    let v = v.trim();
    if let Some(c) = v.strip_prefix('#') {
        return ("", Some(c));
    }
    match v.find(" #") {
        Some(i) => (v[..i].trim(), Some(&v[i + 2..])),
        None => (v, None),
    }
}

/// `[a, b]` → Some(["a", "b"]); not a list → None.
fn list(v: &str) -> Option<Vec<&str>> {
    let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.split(',').map(|s| s.trim().trim_matches(['"', '\'']).trim()).filter(|s| !s.is_empty()).collect())
}

/// Every name a note answers to, normalized: filename, `# Title`, aliases.
fn names(rel: &str, text: &str, aliases: Option<&str>) -> HashSet<String> {
    let mut keys = HashSet::from([name_tokens(&stem(rel)).concat()]);
    keys.extend(aliases.and_then(list).into_iter().flatten().map(|a| name_tokens(a).concat()));
    if let Some(title) = text.lines().find_map(|l| l.strip_prefix("# ")) {
        keys.insert(name_tokens(title).concat());
    }
    keys.remove("");
    keys
}

/// "Home Cooking" → "home-cooking".
fn normalize_tag(t: &str) -> String {
    name_tokens(t).join("-")
}

/// Crude English singular, only for comparing tags: "breads" → "bread", "dishes" → "dish".
fn singular(t: &str) -> String {
    for suffix in ["ies", "es", "s"] {
        if let Some(stem) = t.strip_suffix(suffix) {
            if stem.len() >= 3 && !t.ends_with("ss") {
                return if suffix == "ies" { format!("{stem}y") } else { stem.to_string() };
            }
        }
    }
    t.to_string()
}

/// "Ichiran Namba!" → ["ichiran", "namba"].
fn name_tokens(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + (ca != *cb) as usize).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Targets of [[target]], [[target|alias]], [[target#heading]].
fn links(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(i) = rest.find("[[") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("]]") else { break };
        let target = rest[..j].split(['|', '#']).next().unwrap_or("").trim();
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &rest[j + 2..];
    }
    out
}

fn is_kebab(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn stem(rel: &str) -> String {
    Path::new(rel).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Vault-relative paths of .md files under `dir` ("" = whole vault), skipping dot-dirs (.git, .obsidian…).
fn md_files(root: &Path, dir: &str) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![root.join(dir)];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(files: &[(&str, &str)]) -> tempdir::Dir {
        let dir = tempdir::Dir::new();
        for (rel, text) in files {
            let p = dir.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        dir
    }

    const PLACE: &str = "---\ntype: place\ntags: []\ncity:\nstatus:  # want | visited\nupdated:\n---\n# Name\n";

    fn errors(files: &[(&str, &str)], rel: &str) -> Vec<String> {
        let dir = vault(files);
        Vault::load(&dir.0).check(rel).into_iter().map(|i| i.msg).collect()
    }

    #[test]
    fn valid_note_passes() {
        let note = "---\ntype: place\ntags: [food]\ncity: Osaka\nstatus: visited\nupdated: 2026-10-03\n---\n# Ichiran\n";
        let e = errors(&[("_templates/place.md", PLACE), ("notes/ichiran.md", note)], "notes/ichiran.md");
        assert!(e.is_empty(), "{e:?}");
    }

    #[test]
    fn reports_schema_problems() {
        let note = "---\ntype: place\ntags: [food, Street Food]\nstatus: been\nupdated: yesterday\n---\n[[nowhere]] [[ok|alias]]\n";
        let e = errors(
            &[("_templates/place.md", PLACE), ("notes/Bad_Name.md", note), ("notes/ok.md", "")],
            "notes/Bad_Name.md",
        );
        let all = e.join("\n");
        for want in ["kebab", "tag 'Street Food' should be 'street-food'", "updated must be", "missing field 'city'", "\"been\" is not one of", "broken link [[nowhere]]"] {
            assert!(all.contains(want), "missing {want:?} in:\n{all}");
        }
        assert!(!all.contains("[[ok]]"), "alias link should resolve:\n{all}");
    }

    #[test]
    fn missing_frontmatter_and_unknown_type() {
        assert_eq!(errors(&[("notes/a.md", "# hi\n")], "notes/a.md"), ["missing frontmatter (--- block at the top)"]);
        let e = errors(&[("_templates/place.md", PLACE), ("notes/a.md", "---\ntype: thing\ntags: []\nupdated: 2026-10-03\n---\n")], "notes/a.md");
        assert_eq!(e, ["unknown type 'thing' (one of: place)"]);
    }

    #[test]
    fn orphans_are_warnings() {
        let note = "---\ntype: topic\ntags: []\nupdated: 2026-10-03\n---\n";
        let dir = vault(&[("_templates/topic.md", "---\ntype: topic\ntags: []\nupdated:\n---\n"), ("notes/lonely.md", note), ("notes/linked.md", note), ("me/favorites.md", "---\ntags: []\nupdated: 2026-10-03\n---\n- [[linked]]\n"), ("AGENTS.md", "e.g. [[lonely]]\n")]);
        let issues = Vault::load(&dir.0).check_all();
        let orphans: Vec<_> = issues.iter().filter(|i| i.warn).map(|i| i.path.as_str()).collect();
        assert_eq!(orphans, ["notes/lonely.md"]);
        assert!(issues.iter().all(|i| i.warn), "{}", issues.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn duplicates() {
        let topic = "---\ntype: topic\ntags: []\naliases: []\nupdated:\n---\n";
        let note = |kind: &str, aliases: &str, title: &str| {
            format!("---\ntype: {kind}\ntags: []\naliases: [{aliases}]\nupdated: 2026-10-03\n---\n# {title}\n")
        };
        let dir = vault(&[
            ("_templates/topic.md", topic),
            ("_templates/place.md", topic),
            ("notes/ichiran.md", &note("place", "", "Ichiran")),
            ("notes/ichiran-namba.md", &note("place", "", "Ichiran Namba")),
            ("notes/ramen.md", &note("topic", "", "Ramen")),
            ("notes/ichiran-ramen-guide.md", &note("topic", "", "Guide")), // two extra words: not a duplicate of ramen
            ("notes/piranesi.md", &note("topic", "", "Piranesi")),
            ("notes/piranesy.md", &note("topic", "", "Typo")),
            ("notes/the-house.md", &note("topic", "Piranesi novel", "The House")),
            ("notes/piranesi-novel.md", &note("topic", "", "Other")),
            ("notes/cat.md", &note("topic", "", "Cat")),
            ("notes/cat-food.md", &note("topic", "", "Cat food")), // "cat" too short to count as contained
        ]);
        let v = Vault::load(&dir.0);
        let mut pairs: Vec<String> = v.duplicates(None).iter().map(|i| format!("{} {}", i.path, i.msg)).collect();
        pairs.sort();
        assert_eq!(
            pairs,
            [
                "notes/ichiran-namba.md possible duplicate of [[ichiran]]",
                "notes/piranesi-novel.md possible duplicate of [[piranesi]]",
                "notes/piranesi-novel.md possible duplicate of [[the-house]]",
                "notes/piranesi.md possible duplicate of [[piranesy]]",
            ]
        );
        assert_eq!(v.duplicates(Some(&["notes/piranesy.md".to_string()])).len(), 1);
        // Linking dismisses the warning.
        fs::write(dir.0.join("notes/piranesy.md"), note("topic", "", "Typo") + "Related: [[piranesi]]\n").unwrap();
        assert!(Vault::load(&dir.0).duplicates(Some(&["notes/piranesy.md".to_string()])).is_empty());
    }

    #[test]
    fn content_duplicates_use_vectors() {
        let t = "---\ntype: topic\ntags: []\naliases: []\nupdated:\n---\n";
        let dir = vault(&[("_templates/topic.md", t), ("notes/the-house.md", t), ("notes/piranesi.md", t), ("notes/other.md", t)]);
        let mut v = Vault::load(&dir.0);
        v.dup_threshold = 0.9;
        v.vectors = HashMap::from([
            ("notes/the-house.md".into(), vec![1.0, 0.0]),
            ("notes/piranesi.md".into(), vec![0.99, 0.141]),
            ("notes/other.md".into(), vec![0.0, 1.0]),
        ]);
        let msgs: Vec<String> = v.duplicates(None).iter().map(|i| format!("{} {}", i.path, i.msg)).collect();
        assert_eq!(msgs, ["notes/piranesi.md possible duplicate of [[the-house]] (similar content)"]);
    }

    #[test]
    fn similar_tags_are_warned() {
        let note = |tags: &str| format!("---\ntags: [{tags}]\nupdated: 2026-10-03\n---\n");
        let dir = vault(&[
            ("me/a.md", &note("bread, coffee, glass")),
            ("me/b.md", &note("breads, cofee, dishes")),
            ("me/c.md", &note("bread, dish, class")),
        ]);
        let v = Vault::load(&dir.0);
        assert_eq!(v.tag_counts()[0], ("bread".to_string(), 2));
        let mut msgs: Vec<String> = v.similar_tags().into_iter().map(|i| i.msg).collect();
        msgs.sort();
        assert_eq!(
            msgs,
            [
                "similar tags: bread (2), breads (1): pick one",
                "similar tags: cofee (1), coffee (1): pick one",
                "similar tags: dish (1), dishes (1): pick one",
            ]
        );
    }

    #[test]
    fn recurring_names_without_page() {
        let book = |creator: &str| format!("---\ntype: media\ntags: []\naliases: []\ncreator: {creator}\nupdated: 2026-10-03\n---\n# Book\n");
        let dir = vault(&[
            ("notes/piranesi.md", &book("Susanna Clarke")),
            ("notes/jonathan-strange.md", &book("Susanna Clarke")),
            ("notes/kind-of-blue.md", &book("Miles Davis")),
            ("notes/bitches-brew.md", &book("[[miles-davis]]")),
            ("notes/miles-davis.md", "---\ntype: entity\ntags: []\naliases: [Miles]\nupdated: 2026-10-03\n---\n# Miles Davis\n"),
            ("notes/hail-mary.md", &book("Andy Weir")),
        ]);
        let msgs: Vec<String> = Vault::load(&dir.0).recurring_without_page().into_iter().map(|i| i.msg).collect();
        assert_eq!(msgs, ["recurring 'Susanna Clarke' (2 notes) has no page"]);
    }

    #[test]
    fn list_fields_must_be_lists() {
        let tpl = "---\ntype: topic\ntags: []\naliases: []\nupdated:\n---\n";
        let note = "---\ntype: topic\ntags: []\naliases: Ichiran\nupdated: 2026-10-03\n---\n";
        assert_eq!(errors(&[("_templates/topic.md", tpl), ("notes/a.md", note)], "notes/a.md"), ["aliases must be a [a, b] list, got \"Ichiran\""]);
    }

    #[test]
    fn comments_need_leading_space() {
        assert_eq!(split_comment("  # want | visited"), ("", Some(" want | visited")));
        assert_eq!(split_comment("C# in depth"), ("C# in depth", None));
        assert_eq!(split_comment("Osaka  # city"), ("Osaka", Some(" city")));
    }

    /// Minimal self-cleaning temp dir (no extra dependency).
    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static N: AtomicUsize = AtomicUsize::new(0);
                let p = std::env::temp_dir().join(format!("brain-lint-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
