use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Step-keyed file cache. Used to skip re-running expensive steps when input + config are unchanged.
pub struct StepCache {
    base: PathBuf,
}

impl StepCache {
    pub fn new(cache_dir: &Path) -> Self {
        if let Err(e) = fs::create_dir_all(cache_dir) {
            crate::log_warn!("failed to create cache dir {}: {e}", cache_dir.display());
        }
        Self {
            base: cache_dir.to_path_buf(),
        }
    }

    /// Store a step output. Returns Ok(()) on success, Err on copy failure.
    /// Errors are surfaced (not silently dropped) so a broken cache is visible.
    pub fn store(&self, step: &str, key: &str, source: &Path) -> Result<(), String> {
        let dir = self.base.join(step);
        fs::create_dir_all(&dir)
            .map_err(|e| format!("cache: create_dir {}: {e}", dir.display()))?;
        let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let dst = dir.join(format!("{key}.{ext}"));
        fs::copy(source, &dst).map_err(|e| format!("cache: copy to {}: {e}", dst.display()))?;
        Ok(())
    }

    /// Restore a cached step output. Returns Some(()) if cache hit and copy succeeded.
    ///
    /// On a cache hit we also touch the cached file's mtime to "now". This is
    /// what makes `cache_prune --days N` actually behave like an LRU eviction:
    /// without the touch, mtimes only ever reflect *insertion* time and a
    /// frequently-used entry from a year ago would be evicted alongside cold
    /// entries from the same era.
    pub fn restore(&self, step: &str, key: &str, ext: &str, dest: &Path) -> Option<()> {
        let cached = self.base.join(step).join(format!("{key}.{ext}"));
        if !cached.exists() {
            return None;
        }
        let nonempty = fs::metadata(&cached).map(|m| m.len() > 0).unwrap_or(false);
        if !nonempty {
            return None;
        }
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::copy(&cached, dest).ok()?;
        // Touch mtime to mark "recently used" for LRU. Best-effort: a failure
        // here doesn't change correctness, only prune fairness.
        let now = filetime::FileTime::now();
        let _ = filetime::set_file_mtime(&cached, now);
        Some(())
    }
}

/// Statistics about a cache directory.
#[derive(Debug, Default)]
pub struct CacheStats {
    pub file_count: u64,
    pub total_bytes: u64,
}

/// Walk the cache dir and report total entries + size.
pub fn cache_stats(cache_dir: &Path) -> CacheStats {
    let mut stats = CacheStats::default();
    if !cache_dir.exists() {
        return stats;
    }
    walk_files(cache_dir, &mut |entry, meta| {
        let _ = entry;
        stats.file_count += 1;
        stats.total_bytes += meta.len();
    });
    stats
}

/// Remove all cache files. Returns (removed_count, freed_bytes).
pub fn cache_clean(cache_dir: &Path) -> Result<(u64, u64), String> {
    if !cache_dir.exists() {
        return Ok((0, 0));
    }
    let stats = cache_stats(cache_dir);
    fs::remove_dir_all(cache_dir).map_err(|e| format!("remove cache: {e}"))?;
    fs::create_dir_all(cache_dir).map_err(|e| format!("recreate cache: {e}"))?;
    Ok((stats.file_count, stats.total_bytes))
}

/// Evict cache entries older than `max_age_days` OR until total size ≤ `max_bytes`.
/// Newest entries are kept. Returns (removed_count, freed_bytes).
pub fn cache_prune(
    cache_dir: &Path,
    max_age_days: Option<u64>,
    max_bytes: Option<u64>,
) -> Result<(u64, u64), String> {
    if !cache_dir.exists() {
        return Ok((0, 0));
    }
    let mut entries: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    walk_files(cache_dir, &mut |path, meta| {
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push((path.to_path_buf(), mtime, meta.len()));
    });

    // Newest first
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    let mut removed_count = 0u64;
    let mut freed_bytes = 0u64;

    if let Some(days) = max_age_days {
        let cutoff = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(days * 86400))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        entries.retain(|(path, mtime, size)| {
            if *mtime < cutoff {
                if fs::remove_file(path).is_ok() {
                    removed_count += 1;
                    freed_bytes += size;
                }
                false
            } else {
                true
            }
        });
    }

    if let Some(max) = max_bytes {
        let mut running = 0u64;
        for (path, _mtime, size) in &entries {
            running += size;
            if running > max && fs::remove_file(path).is_ok() {
                removed_count += 1;
                freed_bytes += size;
            }
        }
    }

    Ok((removed_count, freed_bytes))
}

fn walk_files(dir: &Path, callback: &mut dyn FnMut(&Path, &fs::Metadata)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            walk_files(&path, callback);
        } else {
            callback(&path, &meta);
        }
    }
}

/// Build a stable cache key from a fast (path, size, mtime) fingerprint + config.
///
/// Reading a multi-GB video file just to compute a SHA256 cache key is a
/// performance disaster — every `subforge process` invocation would block on
/// hundreds of MB of I/O before even checking whether the cache hit. Instead we
/// fingerprint by:
///   - canonicalized path (so two symlinks to the same file collide harmlessly)
///   - file size in bytes
///   - modification time in nanoseconds since UNIX epoch
///
/// # Trade-off (read this if you build a pipeline on top of subforge)
///
/// The fingerprint is **biased toward false misses, not false hits**:
///
/// - `touch <file>` (mtime change, content unchanged) → cache miss. Costs a
///   re-run; never returns stale data. Acceptable.
/// - Different size, same mtime → cache miss. Same as above.
/// - Same path AND same size AND same mtime → cache hit, regardless of
///   content. Safe in normal use because almost any meaningful edit changes
///   one of size/mtime.
///
/// **Where this can bite you**: automated pipelines that swap a file's
/// contents while preserving size and explicitly resetting mtime (e.g.
/// `cp --preserve=timestamps`, `touch -r ref new`). If your workflow does
/// that, set `--no-cache` on `subforge process`/`translate`, or invalidate
/// the cache directory yourself between runs (`subforge cache clean`).
///
/// We deliberately don't read file contents to compute a content hash — that
/// would multiply the cost of every CLI invocation by gigabyte-scale I/O even
/// when the cache is going to hit. The trade-off is the right one for the
/// 99% case (interactive translation of fresh recordings).
pub fn make_key(input: &Path, config_repr: &str) -> String {
    let file_hash = match fs::metadata(input) {
        Ok(meta) => {
            let canonical = input.canonicalize().unwrap_or_else(|_| input.to_path_buf());
            let size = meta.len();
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut h = Sha256::new();
            h.update(canonical.to_string_lossy().as_bytes());
            h.update(b"|");
            h.update(size.to_le_bytes());
            h.update(b"|");
            h.update(mtime_ns.to_le_bytes());
            hex::encode(h.finalize())
        }
        Err(_) => {
            // Missing input → derive a stable but distinguishable key
            let mut h = Sha256::new();
            h.update(b"__missing__");
            h.update(input.to_string_lossy().as_bytes());
            hex::encode(h.finalize())
        }
    };
    let cfg_hash = hex::encode(Sha256::digest(config_repr.as_bytes()));
    format!("{}_{}", &file_hash[..16], &cfg_hash[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn make_key_stable_for_same_input_and_config() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"hello").unwrap();
        let k1 = make_key(&f, "config=a");
        let k2 = make_key(&f, "config=a");
        assert_eq!(k1, k2);
    }

    #[test]
    fn make_key_differs_when_config_changes() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"hello").unwrap();
        assert_ne!(make_key(&f, "v1"), make_key(&f, "v2"));
    }

    #[test]
    fn make_key_differs_when_file_changes() {
        // The fast fingerprint uses (path, size, mtime). To exercise this
        // without flaky mtime-resolution issues, write contents of different
        // length so size alone forces a different key.
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"hello").unwrap();
        let k1 = make_key(&f, "cfg");
        fs::write(&f, b"hello world").unwrap();
        let k2 = make_key(&f, "cfg");
        assert_ne!(k1, k2);
    }

    #[test]
    fn make_key_does_not_read_file_contents() {
        // Regression: large files used to be fully read into memory to compute
        // a content SHA. Verify that we can fingerprint a "large" file without
        // that cost. Use a 10 MB sparse-ish file and just verify the call
        // returns and is stable when content doesn't change.
        let dir = tempdir().unwrap();
        let f = dir.path().join("big.bin");
        fs::write(&f, vec![0u8; 10 * 1024 * 1024]).unwrap();
        let k1 = make_key(&f, "cfg");
        let k2 = make_key(&f, "cfg");
        assert_eq!(k1, k2);
    }

    #[test]
    fn make_key_handles_missing_input() {
        let missing = Path::new("/nonexistent/path/xyz");
        // Should not panic, should produce a stable key
        let k = make_key(missing, "cfg");
        assert!(!k.is_empty());
    }

    #[test]
    fn store_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let cache = StepCache::new(&dir.path().join("cache"));
        let src = dir.path().join("src.txt");
        fs::write(&src, b"hello").unwrap();
        cache.store("test", "key1", &src).unwrap();

        let dst = dir.path().join("dst.txt");
        assert!(cache.restore("test", "key1", "txt", &dst).is_some());
        assert_eq!(fs::read(&dst).unwrap(), b"hello");
    }

    #[test]
    fn restore_touches_mtime_for_lru() {
        // Regression: cache_prune used to evict frequently-used entries
        // because restore didn't update mtime. Verify mtime advances after
        // restore.
        let dir = tempdir().unwrap();
        let cache = StepCache::new(&dir.path().join("cache"));
        let src = dir.path().join("src.txt");
        fs::write(&src, b"hello").unwrap();
        cache.store("test", "key1", &src).unwrap();

        let cached_path = dir.path().join("cache").join("test").join("key1.txt");
        let mtime_before = fs::metadata(&cached_path).unwrap().modified().unwrap();

        // Make sure enough time elapses for filesystems with second-resolution mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let dst = dir.path().join("dst.txt");
        cache.restore("test", "key1", "txt", &dst).unwrap();
        let mtime_after = fs::metadata(&cached_path).unwrap().modified().unwrap();

        assert!(
            mtime_after > mtime_before,
            "restore should bump cache mtime for LRU; before={mtime_before:?}, after={mtime_after:?}"
        );
    }

    #[test]
    fn restore_returns_none_for_missing() {
        let dir = tempdir().unwrap();
        let cache = StepCache::new(&dir.path().join("cache"));
        let dst = dir.path().join("dst.txt");
        assert!(cache.restore("test", "missing", "txt", &dst).is_none());
    }

    #[test]
    fn restore_returns_none_for_empty_file() {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let step_dir = cache_dir.join("test");
        fs::create_dir_all(&step_dir).unwrap();
        fs::write(step_dir.join("k.txt"), b"").unwrap();

        let cache = StepCache::new(&cache_dir);
        let dst = dir.path().join("dst.txt");
        assert!(cache.restore("test", "k", "txt", &dst).is_none());
    }

    #[test]
    fn cache_stats_counts_files_and_bytes() {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let cache = StepCache::new(&cache_dir);
        let src1 = dir.path().join("a.txt");
        let src2 = dir.path().join("b.txt");
        fs::write(&src1, b"hello").unwrap();
        fs::write(&src2, b"world!!").unwrap();
        cache.store("step1", "k1", &src1).unwrap();
        cache.store("step1", "k2", &src2).unwrap();

        let stats = cache_stats(&cache_dir);
        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.total_bytes, 5 + 7);
    }

    #[test]
    fn cache_clean_removes_everything() {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let cache = StepCache::new(&cache_dir);
        let src = dir.path().join("a.txt");
        fs::write(&src, b"x").unwrap();
        cache.store("s", "k", &src).unwrap();

        let (count, bytes) = cache_clean(&cache_dir).unwrap();
        assert_eq!(count, 1);
        assert_eq!(bytes, 1);

        let stats = cache_stats(&cache_dir);
        assert_eq!(stats.file_count, 0);
    }

    #[test]
    fn cache_prune_by_max_bytes_keeps_newest() {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let cache = StepCache::new(&cache_dir);

        for i in 0..5 {
            let src = dir.path().join(format!("f{i}.txt"));
            fs::write(&src, vec![b'x'; 100]).unwrap();
            cache.store("s", &format!("k{i}"), &src).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Cap at 250 bytes — should keep only newest 2 files (200 bytes total)
        let (removed, freed) = cache_prune(&cache_dir, None, Some(250)).unwrap();
        assert_eq!(removed, 3);
        assert_eq!(freed, 300);

        let stats = cache_stats(&cache_dir);
        assert_eq!(stats.file_count, 2);
    }
}
