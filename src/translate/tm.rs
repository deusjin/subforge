//! Project-level translation memory: glossary + memory persistence + retrieval.
//!
//! `.subforge-tm/memory.jsonl` accumulates source/target pairs with quality scores.
//! We dedup by source (newest wins) and cap to a maximum size to prevent unbounded growth.
//!
//! # Concurrency
//!
//! Two `subforge` invocations may share a `tm_dir` when working on related videos.
//! Both [`save_memory`] and [`crate::translate::quality::save_glossary`] hold an
//! advisory `flock` on a sentinel `.lock` file across the read → merge → write
//! cycle, so concurrent writers are serialized rather than overwriting each other.

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// Maximum entries to keep in memory.jsonl. Older entries are evicted on save.
pub const MAX_MEMORY_ENTRIES: usize = 5000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub source: String,
    pub target: String,
    #[serde(default = "default_score")]
    pub score: f64,
}

fn default_score() -> f64 {
    90.0
}

/// Ensure a TM directory exists at the given path. Does NOT walk up parents
/// (that surprised users by reusing TM dirs from far-away ancestors).
/// If you want shared TM across videos, set `cfg.tm_dir` explicitly.
pub fn ensure_tm_dir(dir: &Path) -> PathBuf {
    let tm_dir = dir.join(".subforge-tm");
    let _ = std::fs::create_dir_all(&tm_dir);
    tm_dir
}

/// Use an explicit path as the TM directory (creating it if needed).
pub fn ensure_tm_dir_at(path: &Path) -> PathBuf {
    let _ = std::fs::create_dir_all(path);
    path.to_path_buf()
}

/// Acquire an exclusive advisory lock on `<tm_dir>/.lock` for the duration of
/// the returned guard. Used to serialize the read-merge-write cycle of
/// memory.jsonl and glossary.jsonl across concurrent processes.
///
/// On platforms without flock (extremely rare), the call still succeeds but
/// no actual locking occurs — fallback semantics are "best effort" rather
/// than "guaranteed serialized", which matches what the old code did.
pub fn lock_tm_dir(tm_dir: &Path) -> std::io::Result<TmLock> {
    std::fs::create_dir_all(tm_dir)?;
    let path = tm_dir.join(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    file.lock_exclusive()?;
    Ok(TmLock { _file: file })
}

/// RAII guard. Drop releases the lock.
pub struct TmLock {
    _file: std::fs::File,
}

impl Drop for TmLock {
    fn drop(&mut self) {
        // fs2 unlocks on file drop; nothing to do explicitly. Keeping the
        // guard ensures the file (and lock) outlives the critical section.
    }
}

/// Load memory entries from .subforge-tm/memory.jsonl.
pub fn load_memory(tm_dir: &Path) -> Vec<MemoryEntry> {
    let path = tm_dir.join("memory.jsonl");
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Merge new entries into existing memory:
/// - Dedup by `source` (newer wins, preserving the highest score).
/// - Cap to MAX_MEMORY_ENTRIES (drop oldest by file order if exceeded).
/// - Atomic write via temp file + rename, under an advisory directory lock.
pub fn save_memory(tm_dir: &Path, new_entries: &[MemoryEntry]) -> Result<usize, String> {
    if new_entries.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(tm_dir).map_err(|e| format!("create_dir {}: {e}", tm_dir.display()))?;

    // Hold the lock across read → merge → write so a concurrent writer can't
    // race in between and lose updates.
    let _lock = lock_tm_dir(tm_dir).map_err(|e| format!("tm lock failed: {e}"))?;

    let existing = load_memory(tm_dir);
    let merged = merge_entries(existing, new_entries);
    let written = merged.len();

    let path = tm_dir.join("memory.jsonl");
    let tmp = tm_dir.join("memory.jsonl.tmp");
    let content: String = merged
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&tmp, content).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(written)
}

/// Merge logic kept pure for testability.
pub fn merge_entries(existing: Vec<MemoryEntry>, new_entries: &[MemoryEntry]) -> Vec<MemoryEntry> {
    // Map source → entry (latest wins, but keep max score)
    let mut by_source: HashMap<String, MemoryEntry> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for e in existing {
        if !by_source.contains_key(&e.source) {
            order.push(e.source.clone());
        }
        by_source.insert(e.source.clone(), e);
    }

    for e in new_entries {
        match by_source.get(&e.source) {
            Some(prev) => {
                // Keep the higher score, otherwise overwrite
                let merged = MemoryEntry {
                    source: e.source.clone(),
                    target: e.target.clone(),
                    score: prev.score.max(e.score),
                };
                by_source.insert(e.source.clone(), merged);
            }
            None => {
                order.push(e.source.clone());
                by_source.insert(e.source.clone(), e.clone());
            }
        }
    }

    // Cap to MAX_MEMORY_ENTRIES (drop oldest)
    let drop_count = order.len().saturating_sub(MAX_MEMORY_ENTRIES);
    let kept_sources = &order[drop_count..];
    kept_sources
        .iter()
        .filter_map(|s| by_source.remove(s))
        .collect()
}

/// Retrieve relevant memory entries for a batch of source texts.
///
/// Performance: builds a HashSet of words per memory entry once instead of
/// calling `Vec::contains` (O(n)) for every word lookup. For 5000 entries
/// and a batch of 7 texts, this is ~30x faster than the previous
/// quadratic-in-words version.
pub fn retrieve_for_batch(
    memory: &[MemoryEntry],
    batch_texts: &[&str],
    max_per_text: usize,
) -> String {
    if memory.is_empty() || batch_texts.is_empty() {
        return String::new();
    }

    // Pre-tokenize each memory entry into a word set.
    let entry_word_sets: Vec<HashSet<&str>> = memory
        .iter()
        .map(|e| e.source.split_whitespace().collect::<HashSet<_>>())
        .collect();

    let mut matches: Vec<(&str, &str)> = Vec::new();
    let mut seen_sources: HashSet<&str> = HashSet::new();

    for text in batch_texts {
        let text_words: Vec<&str> = text.split_whitespace().collect();
        if text_words.len() < 3 {
            continue;
        }
        let denom = text_words.len() as f64;

        let mut scored: Vec<(f64, &MemoryEntry)> = memory
            .iter()
            .zip(entry_word_sets.iter())
            .map(|(entry, ws)| {
                let overlap = text_words.iter().filter(|w| ws.contains(*w)).count();
                let score = overlap as f64 / denom;
                (score, entry)
            })
            .filter(|(score, _)| *score > 0.3)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        for (_, entry) in scored.into_iter().take(max_per_text) {
            if seen_sources.insert(entry.source.as_str()) {
                matches.push((&entry.source, &entry.target));
            }
        }
    }

    if matches.is_empty() {
        return String::new();
    }

    let mut lines = vec!["Reference translations from memory:".to_string()];
    for (src, tgt) in matches.iter().take(5) {
        lines.push(format!("- \"{src}\" → \"{tgt}\""));
    }
    lines.join("\n")
}

/// Format moving window of recently translated cues as context.
pub fn format_moving_window(window: &[(String, String)]) -> String {
    if window.is_empty() {
        return String::new();
    }
    let mut lines = vec!["Recent translations for context continuity:".to_string()];
    for (src, tgt) in window {
        lines.push(format!("- \"{src}\" → \"{tgt}\""));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(source: &str, target: &str, score: f64) -> MemoryEntry {
        MemoryEntry {
            source: source.into(),
            target: target.into(),
            score,
        }
    }

    #[test]
    fn merge_dedups_by_source() {
        let existing = vec![entry("hello", "你好", 80.0)];
        let new = vec![entry("hello", "您好", 90.0)];
        let result = merge_entries(existing, &new);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].target, "您好");
        assert_eq!(result[0].score, 90.0);
    }

    #[test]
    fn merge_keeps_higher_score() {
        let existing = vec![entry("hello", "你好", 95.0)];
        let new = vec![entry("hello", "嗨", 80.0)];
        let result = merge_entries(existing, &new);
        // newer text wins, but score is max
        assert_eq!(result[0].target, "嗨");
        assert_eq!(result[0].score, 95.0);
    }

    #[test]
    fn merge_appends_new_sources() {
        let existing = vec![entry("a", "1", 70.0)];
        let new = vec![entry("b", "2", 80.0), entry("c", "3", 90.0)];
        let result = merge_entries(existing, &new);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn merge_caps_at_max_entries() {
        let mut existing = Vec::new();
        for i in 0..MAX_MEMORY_ENTRIES + 100 {
            existing.push(entry(&format!("src{i}"), &format!("tgt{i}"), 80.0));
        }
        let new = vec![entry("new", "new_t", 90.0)];
        let result = merge_entries(existing, &new);
        assert_eq!(result.len(), MAX_MEMORY_ENTRIES);
        // Newest should be present
        assert!(result.iter().any(|e| e.source == "new"));
    }

    #[test]
    fn retrieve_returns_empty_for_no_matches() {
        let mem = vec![entry("hello world this is a test", "翻译", 90.0)];
        let result = retrieve_for_batch(&mem, &["completely different content"], 2);
        assert!(result.is_empty());
    }

    #[test]
    fn retrieve_finds_overlap() {
        let mem = vec![entry(
            "hello world this is a test",
            "你好世界这是测试",
            90.0,
        )];
        let result = retrieve_for_batch(&mem, &["hello world this is a different test"], 2);
        assert!(result.contains("你好世界"));
    }

    #[test]
    fn save_memory_round_trip_with_lock() {
        let dir = tempfile::tempdir().unwrap();
        save_memory(dir.path(), &[entry("hello", "你好", 90.0)]).unwrap();
        let loaded = load_memory(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].target, "你好");
    }

    #[test]
    fn save_memory_serializes_concurrent_writers() {
        // Two threads writing disjoint entries to the same TM should both
        // survive because the flock serializes their read-merge-write cycle.
        use std::sync::Arc;
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let path = dir.path().to_path_buf();
        let p1 = path.clone();
        let p2 = path.clone();

        let h1 = std::thread::spawn(move || {
            for i in 0..50 {
                save_memory(&p1, &[entry(&format!("a{i}"), &format!("a{i}t"), 80.0)]).unwrap();
            }
        });
        let h2 = std::thread::spawn(move || {
            for i in 0..50 {
                save_memory(&p2, &[entry(&format!("b{i}"), &format!("b{i}t"), 80.0)]).unwrap();
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();

        let loaded = load_memory(&path);
        // We expect 100 unique sources to survive (50 from each thread).
        assert_eq!(
            loaded.len(),
            100,
            "concurrent writes lost entries: got {}",
            loaded.len()
        );
    }
}
