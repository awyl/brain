//! Hybrid search over the vault: full-text (BM25) + semantic (local embeddings), merged by rank fusion.
//!
//! The embedding model is a small BERT downloaded on first use into the cache dir (BRAIN_EMBED=english|multilingual).
//! Vectors live in `.git/brain-index.json` (never committed), updated incrementally by content hash.
//! BRAIN_SEMANTIC=0 turns the semantic half off (no download, full-text only).

use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use candle_core::{Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use serde_json::{Value, json};
use tokenizers::{Tokenizer, TruncationParams};

use crate::Result;

struct Preset {
    name: &'static str,
    repo: &'static str,
    query_prefix: &'static str,
    passage_prefix: &'static str,
    /// CLS-token pooling (bge) vs mean pooling (e5).
    cls: bool,
    /// Cosine similarity above which two same-type notes are flagged as possible duplicates.
    dup_threshold: f32,
    /// Below this, a semantic match is noise and isn't shown...
    min_similarity: f32,
    /// ...and so is anything further than this below the best semantic match (scores sit in a narrow band).
    window: f32,
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "english",
        repo: "BAAI/bge-small-en-v1.5",
        query_prefix: "Represent this sentence for searching relevant passages: ",
        passage_prefix: "",
        cls: true,
        dup_threshold: 0.90,
        min_similarity: 0.58,
        window: 0.05,
    },
    Preset {
        name: "multilingual",
        repo: "intfloat/multilingual-e5-small",
        query_prefix: "query: ",
        passage_prefix: "passage: ",
        cls: false,
        dup_threshold: 0.93,
        min_similarity: 0.80,
        window: 0.03,
    },
];

pub fn semantic_enabled() -> bool {
    env::var("BRAIN_SEMANTIC").map_or(true, |v| v != "0")
}

fn preset() -> Result<&'static Preset> {
    let name = env::var("BRAIN_EMBED").unwrap_or_else(|_| "english".into());
    PRESETS.iter().find(|p| p.name == name).ok_or_else(|| format!("BRAIN_EMBED must be english or multilingual, got {name:?}").into())
}

pub struct Model {
    preset: &'static Preset,
    bert: BertModel,
    tok: Tokenizer,
}

impl Model {
    fn load() -> Result<Self> {
        let preset = preset()?;
        let dir = crate::cache_dir()?.join("models").join(preset.name);
        for file in ["config.json", "tokenizer.json", "model.safetensors"] {
            if !dir.join(file).exists() {
                crate::download(&format!("https://huggingface.co/{}/resolve/main/{file}", preset.repo), &dir.join(file))?;
            }
        }
        let config: Config = serde_json::from_str(&fs::read_to_string(dir.join("config.json"))?)?;
        // Safety: the safetensors file is ours (cache dir) and isn't modified while mapped.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DTYPE, &Device::Cpu)? };
        let bert = BertModel::load(vb, &config)?;
        let mut tok = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| e.to_string())?;
        tok.with_truncation(Some(TruncationParams { max_length: 512, ..Default::default() })).map_err(|e| e.to_string())?;
        tok.with_padding(None);
        Ok(Model { preset, bert, tok })
    }

    /// Unit-length embedding of `text`, as a query or as a stored passage.
    fn embed(&self, text: &str, query: bool) -> Result<Vec<f32>> {
        let prefix = if query { self.preset.query_prefix } else { self.preset.passage_prefix };
        let enc = self.tok.encode(format!("{prefix}{text}"), true).map_err(|e| e.to_string())?;
        let ids = Tensor::new(enc.get_ids(), &Device::Cpu)?.unsqueeze(0)?;
        let mask = Tensor::new(enc.get_attention_mask(), &Device::Cpu)?.unsqueeze(0)?;
        let out = self.bert.forward(&ids, &ids.zeros_like()?, Some(&mask))?.i(0)?; // [tokens, hidden]
        let v = if self.preset.cls { out.i(0)? } else { out.mean(0)? };
        let v = v.broadcast_div(&v.sqr()?.sum_all()?.sqrt()?)?;
        Ok(v.to_vec1::<f32>()?)
    }
}

/// The model, loaded once per process (it's ~130–470MB in memory). A failed load is retried next time.
fn model() -> Result<Arc<Model>> {
    static MODEL: Mutex<Option<Arc<Model>>> = Mutex::new(None);
    let mut m = MODEL.lock().unwrap();
    if m.is_none() {
        *m = Some(Arc::new(Model::load()?));
    }
    Ok(m.clone().unwrap())
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

/// What gets embedded: the body (title + facts) plus aliases. Frontmatter is left out on purpose:
/// shared fields like `city: Osaka` would make every note in a city look alike.
fn semantic_text(text: &str) -> String {
    let (front, body) = split_frontmatter(text);
    let aliases = front.lines().find_map(|l| l.strip_prefix("aliases:")).unwrap_or("").trim();
    let aliases = aliases.trim_start_matches('[').trim_end_matches(']').trim();
    if aliases.is_empty() { body.to_string() } else { format!("{body}\nAlso known as: {aliases}") }
}

fn split_frontmatter(text: &str) -> (&str, &str) {
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
    if preset().ok().map(|p| p.name) != index["model"].as_str() {
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
    let model = model()?;
    // One writer at a time across processes (server, cron, the agent running `brain search`).
    let lock = File::create(vault.join(".git/brain-index.lock"))?;
    lock.lock()?;
    let old: Value = fs::read_to_string(index_path(vault)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let same_model = old["model"].as_str() == Some(model.preset.name);

    let (mut vectors, mut entries, mut embedded) = (HashMap::new(), serde_json::Map::new(), 0);
    for d in docs(vault) {
        let hash = fnv(&d.text);
        let old_entry = &old["entries"][&d.rel];
        let v = if same_model && old_entry["hash"].as_u64() == Some(hash) {
            vector(&old_entry["vec"])
        } else {
            embedded += 1;
            model.embed(&semantic_text(&d.text), false)?
        };
        entries.insert(d.rel.clone(), json!({ "hash": hash, "vec": v }));
        vectors.insert(d.rel, v);
    }
    if embedded > 0 || entries.len() != old["entries"].as_object().map_or(0, |o| o.len()) {
        let tmp = vault.join(".git/brain-index.json.part");
        fs::write(&tmp, json!({ "model": model.preset.name, "entries": entries }).to_string())?;
        fs::rename(tmp, index_path(vault))?;
    }
    Ok((vectors, embedded))
}

pub fn dup_threshold() -> f32 {
    let default = preset().map_or(1.1, |p| p.dup_threshold);
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

/// Top `k` files for `query`, best first.
pub fn search(vault: &Path, query: &str, k: usize) -> Result<Vec<Hit>> {
    let docs = docs(vault);
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
    if semantic_enabled() {
        let (vectors, _) = update_index(vault)?;
        let qv = model()?.embed(query, true)?;
        let p = preset()?;
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
    fn tokenizes_words_and_cjk() {
        assert_eq!(tokens("Ichiran Namba, 一蘭 ramen!"), ["ichiran", "namba", "一", "蘭", "ramen"]);
    }

    #[test]
    fn semantic_text_skips_frontmatter_keeps_aliases() {
        let t = "---\ntype: place\naliases: [Ichiran]\ncity: Osaka\n---\n# Ichiran Namba\n- 2026-10-03 · good\n";
        assert_eq!(semantic_text(t), "# Ichiran Namba\n- 2026-10-03 · good\n\nAlso known as: Ichiran");
    }

    #[test]
    fn snippet_prefers_matching_line() {
        let t = "---\na: b\n---\n# T\n- first fact\n- open 24 hours\n";
        assert_eq!(snippet(t, &tokens("hours")), "open 24 hours");
        assert_eq!(snippet(t, &tokens("zzz")), "first fact");
    }
}
