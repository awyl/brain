//! Hybrid search over the vault: full-text (BM25) + semantic (embeddings), merged by rank fusion.
//!
//! BRAIN_EMBED=<aiproxy embedding model> turns the semantic half on, e.g. `embeddings-local/bge-small-en-v1.5`
//! (aiproxy runs it locally on CPU). Unset: full-text only.
//! Vectors live in `.git/brain-index.json` (never committed), updated incrementally by content hash.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Result;
use crate::aiproxy::{Proxy, nonempty};

/// Texts per embeddings request when indexing.
const BATCH: usize = 32;

struct Preset {
    /// Matched against the model id.
    family: &'static str,
    query_prefix: &'static str,
    passage_prefix: &'static str,
    /// Cosine similarity above which two same-type notes are flagged as possible duplicates.
    dup_threshold: f32,
    /// Below this, a semantic match is noise and isn't shown...
    min_similarity: f32,
    /// ...and so is anything further than this below the best semantic match (scores sit in a narrow band).
    window: f32,
}

/// Per model family: the prefixes it was trained with, and score bands tuned on a real vault (bge, e5).
/// The last entry is the fallback: no prefixes, bge's bands; tune with BRAIN_MIN_SIM / BRAIN_DUP_SIM.
const PRESETS: &[Preset] = &[
    Preset {
        family: "bge",
        query_prefix: "Represent this sentence for searching relevant passages: ",
        passage_prefix: "",
        dup_threshold: 0.90,
        min_similarity: 0.58,
        window: 0.05,
    },
    Preset { family: "e5", query_prefix: "query: ", passage_prefix: "passage: ", dup_threshold: 0.93, min_similarity: 0.80, window: 0.03 },
    Preset {
        family: "nomic",
        query_prefix: "search_query: ",
        passage_prefix: "search_document: ",
        dup_threshold: 0.90,
        min_similarity: 0.58,
        window: 0.05,
    },
    Preset { family: "", query_prefix: "", passage_prefix: "", dup_threshold: 0.90, min_similarity: 0.58, window: 0.05 },
];

fn embed_model() -> Option<String> {
    nonempty("BRAIN_EMBED")
}

pub fn semantic_enabled() -> bool {
    embed_model().is_some()
}

fn preset(model: &str) -> &'static Preset {
    PRESETS.iter().find(|p| model.contains(p.family)).unwrap()
}

/// Unit-length embeddings of `texts` from aiproxy's /v1/embeddings, as queries or as stored passages.
fn embed(model: &str, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
    let p = preset(model);
    let prefix = if query { p.query_prefix } else { p.passage_prefix };
    let input: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
    let body = json!({ "model": model, "input": input });
    let v: Value = serde_json::from_reader(Proxy::from_env().post("/v1/embeddings", &body, &[])?.into_reader())?;
    let mut data = v["data"].as_array().cloned().unwrap_or_default();
    data.sort_by_key(|d| d["index"].as_u64());
    if data.len() != texts.len() {
        return Err(format!("embeddings: sent {} texts, got {} vectors", texts.len(), data.len()).into());
    }
    Ok(data
        .iter()
        .map(|d| {
            let v = vector(&d["embedding"]);
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(f32::EPSILON);
            v.into_iter().map(|x| x / norm).collect()
        })
        .collect())
}

/// A searchable file: vault-relative path and contents.
struct Doc {
    rel: String,
    text: String,
}

fn docs(vault: &Path) -> Vec<Doc> {
    ["notes", "me", "inbox"]
        .iter()
        .flat_map(|d| fs::read_dir(vault.join(d)).into_iter().flatten().flatten())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| {
            let rel = e.path().strip_prefix(vault).ok()?.to_string_lossy().into_owned();
            Some(Doc { text: fs::read_to_string(e.path()).ok()?, rel })
        })
        .collect()
}

/// Tagged `private`: never leaves the vault through `brain mcp`.
pub fn is_private(text: &str) -> bool {
    let (front, _) = split_frontmatter(text);
    front.lines().find_map(|l| l.strip_prefix("tags:")).is_some_and(|t| {
        t.trim().trim_matches(['[', ']']).split(',').any(|tag| tag.trim() == "private")
    })
}

/// What gets embedded: the body (title + facts) plus aliases. Frontmatter is left out on purpose:
/// shared fields like `city: Osaka` would make every note in a city look alike.
fn semantic_text(text: &str) -> String {
    let (front, body) = split_frontmatter(text);
    let aliases = front.lines().find_map(|l| l.strip_prefix("aliases:")).unwrap_or("").trim();
    let aliases = aliases.trim_start_matches('[').trim_end_matches(']').trim();
    if aliases.is_empty() { body.to_string() } else { format!("{body}\nAlso known as: {aliases}") }
}

pub fn split_frontmatter(text: &str) -> (&str, &str) {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(i) = rest.find("\n---") {
            let body = rest[i + 4..].trim_start_matches(|c| c != '\n').trim_start();
            return (&rest[..i], body);
        }
    }
    ("", text)
}

/// FNV-1a: stable across builds (unlike std's hasher), good enough to spot changed files.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn index_path(vault: &Path) -> PathBuf {
    vault.join(".git/brain-index.json")
}

/// Stored vectors whose file content hasn't changed since they were computed. Never embeds anything.
pub fn fresh_vectors(vault: &Path) -> HashMap<String, Vec<f32>> {
    let Ok(index) = fs::read_to_string(index_path(vault)).map(|s| serde_json::from_str::<Value>(&s).unwrap_or_default()) else {
        return HashMap::new();
    };
    if embed_model().as_deref() != index["model"].as_str() {
        return HashMap::new();
    }
    docs(vault)
        .into_iter()
        .filter_map(|d| {
            let e = &index["entries"][&d.rel];
            (e["hash"].as_u64() == Some(fnv(&d.text))).then(|| (d.rel, vector(&e["vec"])))
        })
        .collect()
}

fn vector(v: &Value) -> Vec<f32> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()
}

/// Embed new or changed files, drop deleted ones. Returns (vectors, how many were embedded).
pub fn update_index(vault: &Path) -> Result<(HashMap<String, Vec<f32>>, usize)> {
    let model = embed_model().ok_or("BRAIN_EMBED is not set")?;
    // One writer at a time across processes (server, cron, the agent running `brain search`).
    let lock = File::create(vault.join(".git/brain-index.lock"))?;
    lock.lock()?;
    let old: Value = fs::read_to_string(index_path(vault)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let same_model = old["model"].as_str() == Some(&model);

    let docs = docs(vault);
    let (keep, todo): (Vec<&Doc>, Vec<&Doc>) =
        docs.iter().partition(|d| same_model && old["entries"][&d.rel]["hash"].as_u64() == Some(fnv(&d.text)));
    let mut vectors: HashMap<String, Vec<f32>> = keep.iter().map(|d| (d.rel.clone(), vector(&old["entries"][&d.rel]["vec"]))).collect();
    for chunk in todo.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|d| semantic_text(&d.text)).collect();
        vectors.extend(chunk.iter().map(|d| d.rel.clone()).zip(embed(&model, &texts, false)?));
    }
    let entries: serde_json::Map<String, Value> =
        docs.iter().map(|d| (d.rel.clone(), json!({ "hash": fnv(&d.text), "vec": vectors[&d.rel] }))).collect();
    if !todo.is_empty() || entries.len() != old["entries"].as_object().map_or(0, |o| o.len()) {
        let tmp = vault.join(".git/brain-index.json.part");
        fs::write(&tmp, json!({ "model": model, "entries": entries }).to_string())?;
        fs::rename(tmp, index_path(vault))?;
    }
    Ok((vectors, todo.len()))
}

pub fn dup_threshold() -> f32 {
    let default = embed_model().map_or(1.1, |m| preset(&m).dup_threshold);
    env::var("BRAIN_DUP_SIM").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub struct Hit {
    pub rel: String,
    pub title: String,
    pub snippet: String,
    /// Why it matched, e.g. "text 2.1, meaning 0.74".
    pub how: String,
}

/// Top `k` files for `query`, best first. `include_private: false` skips notes tagged `private`.
pub fn search(vault: &Path, query: &str, k: usize, include_private: bool) -> Result<Vec<Hit>> {
    let docs: Vec<Doc> = docs(vault).into_iter().filter(|d| include_private || !is_private(&d.text)).collect();
    let q: Vec<String> = tokens(query).into_iter().filter(|t| !STOPWORDS.contains(&t.as_str())).collect();

    // Full text: BM25 over frontmatter values (not keys) and body; title and aliases count three times.
    let doc_tokens: Vec<Vec<String>> = docs
        .iter()
        .map(|d| {
            let (front, body) = split_frontmatter(&d.text);
            // Values only, minus `type`/`updated`: "place" or a date in a query shouldn't match every note's structure.
            let values: Vec<&str> = front
                .lines()
                .filter_map(|l| l.split_once(':'))
                .filter(|(k, _)| !matches!(k.trim(), "type" | "updated"))
                .map(|(_, v)| v)
                .collect();
            let aliases = front.lines().find_map(|l| l.strip_prefix("aliases:")).unwrap_or("");
            let names = format!("{} {aliases}", title(&d.text));
            let mut t = tokens(&format!("{}\n{body}\n{names}\n{names}\n{names}", values.join(" ")));
            t.extend(tokens(&d.rel));
            t
        })
        .collect();
    let avg = doc_tokens.iter().map(Vec::len).sum::<usize>() as f32 / docs.len().max(1) as f32;
    let df = |t: &String| doc_tokens.iter().filter(|d| d.contains(t)).count() as f32;
    let n = docs.len() as f32;
    let idf: HashMap<&String, f32> = q.iter().map(|t| (t, ((n - df(t) + 0.5) / (df(t) + 0.5) + 1.0).ln())).collect();
    let mut text_rank: Vec<(usize, f32)> = doc_tokens
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let len = d.len() as f32;
            let score = q.iter().map(|t| {
                let tf = d.iter().filter(|x| *x == t).count() as f32;
                idf[t] * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * len / avg))
            });
            (i, score.sum())
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    text_rank.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Meaning: cosine similarity against the stored vectors.
    let mut meaning_rank: Vec<(usize, f32)> = vec![];
    if let Some(model) = embed_model() {
        let (vectors, _) = update_index(vault)?;
        let qv = embed(&model, &[query.to_string()], true)?.remove(0);
        let p = preset(&model);
        let floor = env::var("BRAIN_MIN_SIM").ok().and_then(|v| v.parse().ok()).unwrap_or(p.min_similarity);
        meaning_rank = docs.iter().enumerate().filter_map(|(i, d)| Some((i, cosine(&qv, vectors.get(&d.rel)?)))).collect();
        meaning_rank.sort_by(|a, b| b.1.total_cmp(&a.1));
        let best = meaning_rank.first().map_or(0.0, |m| m.1);
        meaning_rank.retain(|(_, c)| *c >= floor && *c >= best - p.window);
        meaning_rank.truncate(k * 2);
    }

    // Reciprocal rank fusion: robust without having to calibrate the two score scales against each other.
    let mut fused: HashMap<usize, (f32, Vec<String>)> = HashMap::new();
    for (rank, (i, score)) in text_rank.iter().take(k * 2).enumerate() {
        let e = fused.entry(*i).or_default();
        e.0 += 1.0 / (60.0 + rank as f32);
        e.1.push(format!("text {score:.1}"));
    }
    for (rank, (i, score)) in meaning_rank.iter().enumerate() {
        let e = fused.entry(*i).or_default();
        e.0 += 1.0 / (60.0 + rank as f32);
        e.1.push(format!("meaning {score:.2}"));
    }
    let mut fused: Vec<_> = fused.into_iter().collect();
    fused.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    Ok(fused
        .into_iter()
        .take(k)
        .map(|(i, (_, how))| {
            let d = &docs[i];
            Hit { rel: d.rel.clone(), title: title(&d.text), snippet: snippet(&d.text, &q), how: how.join(", ") }
        })
        .collect())
}

/// Words too common to mean anything in a query.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "at", "be", "by", "did", "do", "for", "from", "have", "how", "i", "in", "is", "it", "me",
    "my", "of", "on", "or", "that", "the", "this", "to", "was", "what", "where", "which", "who", "with", "you",
];

/// Lowercase words; CJK characters count one each (no spaces to split on).
fn tokens(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut word = String::new();
    for c in s.to_lowercase().chars() {
        let cjk = matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF);
        if c.is_alphanumeric() && !cjk {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if cjk {
            out.push(c.to_string());
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn title(text: &str) -> String {
    text.lines().find_map(|l| l.strip_prefix("# ")).unwrap_or("").trim().to_string()
}

/// The body line sharing the most words with the query, else the first fact.
fn snippet(text: &str, q: &[String]) -> String {
    let (_, body) = split_frontmatter(text);
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let best = lines
        .iter()
        .max_by_key(|l| tokens(l).iter().filter(|t| q.contains(t)).count())
        .filter(|l| tokens(l).iter().any(|t| q.contains(t)))
        .or(lines.first())
        .unwrap_or(&"");
    best.trim_start_matches("- ").chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_by_model_family() {
        assert_eq!(preset("embeddings-local/bge-small-en-v1.5").family, "bge");
        assert_eq!(preset("embeddings-local/nomic-embed-text-v1.5").family, "nomic");
        assert_eq!(preset("embeddings-local/all-MiniLM-L6-v2").family, "");
    }

    #[test]
    fn tokenizes_words_and_cjk() {
        assert_eq!(tokens("Ichiran Namba, 一蘭 ramen!"), ["ichiran", "namba", "一", "蘭", "ramen"]);
    }

    #[test]
    fn semantic_text_skips_frontmatter_keeps_aliases() {
        let t = "---\ntype: place\naliases: [Ichiran]\ncity: Osaka\n---\n# Ichiran Namba\n- 2026-10-03 · good\n";
        assert_eq!(semantic_text(t), "# Ichiran Namba\n- 2026-10-03 · good\n\nAlso known as: Ichiran");
    }

    #[test]
    fn private_tag() {
        assert!(is_private("---\ntags: [health, private]\n---\n# x\n"));
        assert!(!is_private("---\ntags: [privateer]\n---\n# x\n"));
        assert!(!is_private("# no frontmatter, tags: [private]\n"));
    }

    #[test]
    fn snippet_prefers_matching_line() {
        let t = "---\na: b\n---\n# T\n- first fact\n- open 24 hours\n";
        assert_eq!(snippet(t, &tokens("hours")), "open 24 hours");
        assert_eq!(snippet(t, &tokens("zzz")), "first fact");
    }
}
