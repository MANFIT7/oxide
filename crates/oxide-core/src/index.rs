//! Fast persistent codebase index (Augment-style "context engine", lite).
//!
//! Instead of re-scanning the whole repo on every `codebase_search`, we keep a
//! per-file chunk index on disk (`.oxide/index/code.json`) and refresh only the
//! files whose mtime changed. Ranking is TF-IDF over ~60-line chunks with a
//! symbol-name and path-name boost — semantic-ish and instant after the first
//! build. (True embeddings can layer on top of this later.)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const CHUNK_LINES: usize = 60;
const CHUNK_STEP: usize = 50;
const MAX_FILES: usize = 4000;
const MAX_FILE_BYTES: u64 = 400_000;

#[derive(Serialize, Deserialize, Default)]
pub struct CodeIndex {
    files: HashMap<String, FileEntry>,
}

#[derive(Serialize, Deserialize)]
struct FileEntry {
    mtime: u64,
    chunks: Vec<Chunk>,
}

#[derive(Serialize, Deserialize)]
struct Chunk {
    start: usize, // 1-based first line
    text: String,
    terms: HashMap<String, u32>, // term -> term-frequency
    symbols: Vec<String>,        // declarations on these lines
}

/// Tokenise into lowercase words >2 chars, splitting identifiers on
/// camelCase / snake_case so `codebaseSearch` matches `codebase` + `search`.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in s.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if raw.is_empty() {
            continue;
        }
        // snake parts
        for part in raw.split('_') {
            if part.is_empty() {
                continue;
            }
            // camelCase split
            let mut cur = String::new();
            let mut prev_lower = false;
            for ch in part.chars() {
                if ch.is_uppercase() && prev_lower && !cur.is_empty() {
                    if cur.len() > 2 {
                        out.push(cur.to_lowercase());
                    }
                    cur.clear();
                }
                prev_lower = ch.is_lowercase();
                cur.push(ch);
            }
            if cur.len() > 2 {
                out.push(cur.to_lowercase());
            }
            // also keep the whole part (e.g. numbers, short keywords joined)
            if part.len() > 2 {
                out.push(part.to_lowercase());
            }
        }
    }
    out
}

/// Detect a top-level declaration name on a line (best-effort, multi-language).
fn symbol_on(line: &str) -> Option<String> {
    let t = line.trim_start();
    for kw in [
        "fn ", "struct ", "enum ", "trait ", "impl ", "type ", "const ", "static ",
        "class ", "def ", "function ", "interface ", "func ", "public ", "private ",
    ] {
        if let Some(rest) = t.strip_prefix(kw) {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.len() > 1 {
                return Some(name);
            }
        }
    }
    None
}

fn chunk_file(text: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let end = (start + CHUNK_LINES).min(lines.len());
        let body = lines[start..end].join("\n");
        let mut terms: HashMap<String, u32> = HashMap::new();
        for tk in tokenize(&body) {
            *terms.entry(tk).or_insert(0) += 1;
        }
        let symbols: Vec<String> = lines[start..end].iter().filter_map(|l| symbol_on(l)).collect();
        chunks.push(Chunk { start: start + 1, text: body, terms, symbols });
        if end == lines.len() {
            break;
        }
        start += CHUNK_STEP;
    }
    chunks
}

fn collect(root: &Path, out: &mut Vec<std::path::PathBuf>) {
    if out.len() > MAX_FILES {
        return;
    }
    const SKIP: &[&str] = &[
        ".git", "target", "node_modules", ".oxide", "dist", "build", ".next",
        "vendor", ".venv", "__pycache__", ".cache", "out",
    ];
    const EXT: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "c", "h",
        "cpp", "hpp", "rb", "php", "swift", "kt", "cs", "scala", "sh", "toml", "md",
        "css", "html", "vue", "svelte", "sql",
    ];
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".cursorrules" {
            continue;
        }
        if p.is_dir() {
            if !SKIP.contains(&name.as_str()) {
                collect(&p, out);
            }
        } else if p.extension().and_then(|x| x.to_str()).map(|x| EXT.contains(&x)).unwrap_or(false) {
            out.push(p);
        }
    }
}

fn mtime_of(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Incrementally bring the index up to date with the workspace.
fn update(ws: &Path, idx: &mut CodeIndex) {
    let mut files = Vec::new();
    collect(ws, &mut files);
    let mut present = std::collections::HashSet::new();
    for f in &files {
        let rel = f.strip_prefix(ws).unwrap_or(f).to_string_lossy().replace('\\', "/");
        present.insert(rel.clone());
        let mt = mtime_of(f);
        if idx.files.get(&rel).map(|e| e.mtime) == Some(mt) {
            continue; // unchanged
        }
        if std::fs::metadata(f).map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
            idx.files.remove(&rel);
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(f) {
            // Skip minified/generated files (one giant line) — they're not source
            // the user searches and they swamp ranking with token soup.
            if text.lines().any(|l| l.len() > 5000) {
                idx.files.remove(&rel);
                continue;
            }
            idx.files.insert(rel, FileEntry { mtime: mt, chunks: chunk_file(&text) });
        }
    }
    // Drop files that no longer exist.
    idx.files.retain(|k, _| present.contains(k));
}

/// Rank chunks by TF-IDF + symbol/path boosts; return formatted top snippets.
fn query(idx: &CodeIndex, q: &str) -> String {
    let terms: Vec<String> = {
        let mut t = tokenize(q);
        t.sort();
        t.dedup();
        t
    };
    if terms.is_empty() {
        return "codebase_search: provide a query of 1+ words".to_string();
    }
    let n_chunks: usize = idx.files.values().map(|f| f.chunks.len()).sum::<usize>().max(1);
    // document frequency per term
    let mut df: HashMap<&str, u32> = HashMap::new();
    for f in idx.files.values() {
        for c in &f.chunks {
            for t in &terms {
                if c.terms.contains_key(t) {
                    *df.entry(t.as_str()).or_insert(0) += 1;
                }
            }
        }
    }
    let mut hits: Vec<(f64, &str, usize, &str)> = Vec::new();
    for (path, f) in &idx.files {
        let path_l = path.to_lowercase();
        for c in &f.chunks {
            let mut score = 0.0f64;
            for t in &terms {
                if let Some(&tf) = c.terms.get(t) {
                    let d = *df.get(t.as_str()).unwrap_or(&1) as f64;
                    let idf = ((n_chunks as f64) / d).ln().max(0.2);
                    score += (tf as f64) * idf;
                }
                // symbol exact-name boost
                if c.symbols.iter().any(|s| s.to_lowercase() == *t) {
                    score += 8.0;
                }
                // path-name boost
                if path_l.contains(t.as_str()) {
                    score += 2.0;
                }
            }
            if score > 0.0 {
                hits.push((score, path.as_str(), c.start, c.text.as_str()));
            }
        }
    }
    if hits.is_empty() {
        return format!("No code found for: {}", terms.join(" "));
    }
    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    hits.dedup_by(|a, b| a.1 == b.1 && a.2.abs_diff(b.2) < 20);
    // Hybrid: TF-IDF prefilters the top candidates, then local semantic
    // embeddings rerank them by meaning (falls back to TF-IDF order offline).
    hits.truncate(50);
    let texts: Vec<String> = hits.iter().map(|h| h.3.to_string()).collect();
    let order: Vec<usize> = crate::embed::rerank(q, &texts)
        .unwrap_or_else(|| (0..hits.len()).collect());
    let mut out = String::new();
    for &i in order.iter().take(8) {
        let (_, path, line, text) = &hits[i];
        let snippet: String = text.lines().take(18).collect::<Vec<_>>().join("\n");
        out.push_str(&format!("── {path}:{line}\n{snippet}\n\n"));
    }
    out.chars().take(9000).collect()
}

/// Load the on-disk index, refresh changed files, run the query, and persist.
/// Returns the formatted snippet result.
pub fn search(ws: &Path, q: &str) -> String {
    let dir = ws.join(".oxide/index");
    let path = dir.join("code.json");
    let mut idx: CodeIndex = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    update(ws, &mut idx);
    let result = query(&idx, q);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(b) = serde_json::to_vec(&idx) {
        let _ = std::fs::write(&path, b);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_relevant_code() {
        let ws = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let out = search(ws, "persistent codebase index search");
        assert!(out.contains("index.rs") || out.contains(".rs:"), "got: {}", &out[..out.len().min(200)]);
    }
}
