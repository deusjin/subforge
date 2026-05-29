//! LLM-based segmentation polish layer.
//!
//! After SaT segmentation, optionally ask an LLM to identify pairs of adjacent
//! segments that appear to be wrongly split (mid-clause). When found, merge them.
//! McCarthy et al. (Findings of EMNLP 2023) idea adapted for subtitles.
//!
//! Triggered only when `polish_with_llm = true` in config.

use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::translate::llm::call_chat_with_system;
use crate::util::Segment;

const CHUNK_SIZE: usize = 20;
const OVERLAP: usize = 1;

/// Polish segmentation by asking the LLM which adjacent pairs to merge.
/// Processes in overlapping chunks to avoid LLM timeouts.
pub async fn polish_segments(segments: &[Segment], cfg: &Config) -> Vec<Segment> {
    let api_key = cfg.polish_api_key().to_string();
    let base_url = cfg.polish_base_url().to_string();
    let model = cfg.polish_model_or_default();
    if !cfg.polish_with_llm || segments.len() < 2 || api_key.is_empty() || base_url.is_empty() {
        return segments.to_vec();
    }

    crate::log_info!("  polishing segment boundaries with LLM...");

    let merge_limit = (cfg.max_chars_per_cue as f32 * 1.5) as u16;
    let chunks = build_chunks(segments);
    let total_chunks = chunks.len();

    let semaphore = Arc::new(Semaphore::new(cfg.safe_thread_num()));
    let mut handles = Vec::new();

    for chunk in chunks {
        let sem = semaphore.clone();
        let api_key = api_key.clone();
        let base_url = base_url.clone();
        let model = model.clone();
        let chunk_data: Vec<(usize, String)> =
            chunk.into_iter().map(|s| (s.index, s.text)).collect();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            polish_chunk(&chunk_data, &api_key, &base_url, &model, merge_limit).await
        }));
    }

    let mut all_pairs: Vec<(usize, usize)> = Vec::new();
    let mut completed = 0;
    for handle in handles {
        if let Ok(pairs) = handle.await {
            all_pairs.extend(pairs);
        }
        completed += 1;
        crate::log_debug!("polish {completed}/{total_chunks} done");
    }

    if all_pairs.is_empty() {
        crate::log_info!("  no merges suggested");
        return segments.to_vec();
    }

    all_pairs.sort();
    all_pairs.dedup();
    crate::log_info!("  merging {} boundary pairs", all_pairs.len());

    apply_merges(segments, &all_pairs)
}

/// Build overlapping chunks for chunked LLM processing.
/// Pure function for testability.
fn build_chunks(segments: &[Segment]) -> Vec<Vec<Segment>> {
    let mut chunks: Vec<Vec<Segment>> = Vec::new();
    let mut start = 0;
    while start < segments.len() {
        let end = (start + CHUNK_SIZE).min(segments.len());
        chunks.push(segments[start..end].to_vec());
        if end == segments.len() {
            break;
        }
        start = end - OVERLAP;
    }
    chunks
}

async fn polish_chunk(
    chunk: &[(usize, String)],
    api_key: &str,
    base_url: &str,
    model: &str,
    merge_limit: u16,
) -> Vec<(usize, usize)> {
    if chunk.len() < 2 {
        return Vec::new();
    }

    let lines: Vec<Value> = chunk
        .iter()
        .map(|(i, t)| json!({"i": i, "text": t}))
        .collect();
    let input = serde_json::to_string(&lines).unwrap_or_default();

    let system = format!(
        "You are reviewing automatic subtitle segmentation for a video.\n\
        Each segment is one cue shown on screen. A good cue ends at a complete clause boundary,\n\
        not in the middle of a phrase.\n\
        \n\
        For each adjacent pair (i, i+1), decide if they should be merged. MERGE when:\n\
        - Segment i ends mid-phrase (on a determiner like 'the/a/our', conjunction 'and/or/to',\n\
          preposition 'of/in/on', or part of a compound noun like 'gameplay'/'damage')\n\
        - Segment i+1 starts with the continuation (verb, NP completion, etc.)\n\
        - Merged length is ≤ {merge_limit} chars\n\
        \n\
        Examples that SHOULD merge:\n\
        - i='...our Aura damage', i+1='gameplay ability needs...'\n\
        - i='...set on the gameplay', i+1='ability itself...'\n\
        - i='...if you wanted this to scale', i+1='by level, you would make...'\n\
        \n\
        Input: JSON array of {{\"i\": int, \"text\": str}}.\n\
        Output: ONLY a JSON array of [i, i+1] pairs to merge, e.g. [[3,4],[7,8]].\n\
        If nothing to merge, return []. No prose, no markdown."
    );

    let response = match call_chat_with_system(api_key, base_url, model, &system, &input).await {
        Ok(r) => r,
        Err(e) => {
            let preview: String = e.chars().take(120).collect();
            crate::log_debug!("polish chunk failed: {preview}");
            return Vec::new();
        }
    };

    parse_merge_pairs(&response)
}

fn parse_merge_pairs(raw: &str) -> Vec<(usize, usize)> {
    let try_parse = |s: &str| -> Option<Vec<(usize, usize)>> {
        let arr: Vec<Vec<usize>> = serde_json::from_str(s).ok()?;
        Some(
            arr.into_iter()
                .filter_map(|v| {
                    if v.len() == 2 && v[1] == v[0] + 1 {
                        Some((v[0], v[1]))
                    } else {
                        None
                    }
                })
                .collect(),
        )
    };
    if let Some(pairs) = try_parse(raw.trim()) {
        return pairs;
    }
    if let Some(start) = raw.find('[')
        && let Some(end) = raw.rfind(']')
        && end > start
    {
        return try_parse(&raw[start..=end]).unwrap_or_default();
    }
    Vec::new()
}

/// Pure merge function: given segments and pairs, produce merged segments.
fn apply_merges(segments: &[Segment], pairs: &[(usize, usize)]) -> Vec<Segment> {
    use std::collections::HashSet;
    let merge_targets: HashSet<usize> = pairs.iter().map(|(_, b)| *b).collect();

    let mut result: Vec<(u64, u64, String)> = Vec::new();
    for seg in segments {
        if merge_targets.contains(&seg.index)
            && let Some(last) = result.last_mut()
        {
            last.1 = seg.end_ms;
            last.2 = format!("{} {}", last.2.trim_end(), seg.text.trim_start());
            continue;
        }
        result.push((seg.start_ms, seg.end_ms, seg.text.clone()));
    }

    result
        .into_iter()
        .enumerate()
        .map(|(idx, (s, e, t))| Segment {
            index: idx + 1,
            start_ms: s,
            end_ms: e,
            text: t,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(i: usize, start: u64, end: u64, text: &str) -> Segment {
        Segment {
            index: i,
            start_ms: start,
            end_ms: end,
            text: text.into(),
        }
    }

    #[test]
    fn parse_merge_pairs_plain_array() {
        let r = parse_merge_pairs("[[1,2],[3,4]]");
        assert_eq!(r, vec![(1, 2), (3, 4)]);
    }

    #[test]
    fn parse_merge_pairs_embedded() {
        let r = parse_merge_pairs("here are pairs: [[5,6]] cool");
        assert_eq!(r, vec![(5, 6)]);
    }

    #[test]
    fn parse_merge_pairs_filters_non_adjacent() {
        let r = parse_merge_pairs("[[1,3]]");
        assert!(r.is_empty());
    }

    #[test]
    fn parse_merge_pairs_empty() {
        assert!(parse_merge_pairs("[]").is_empty());
        assert!(parse_merge_pairs("not json").is_empty());
    }

    #[test]
    fn apply_merges_basic() {
        let segments = vec![
            seg(1, 0, 1000, "hello"),
            seg(2, 1000, 2000, "world"),
            seg(3, 2000, 3000, "foo"),
        ];
        let pairs = vec![(1, 2)]; // merge segment 1 with 2
        let result = apply_merges(&segments, &pairs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "hello world");
        assert_eq!(result[0].start_ms, 0);
        assert_eq!(result[0].end_ms, 2000);
        assert_eq!(result[1].text, "foo");
        assert_eq!(result[0].index, 1);
        assert_eq!(result[1].index, 2);
    }

    #[test]
    fn apply_merges_chained() {
        let segments = vec![
            seg(1, 0, 100, "a"),
            seg(2, 100, 200, "b"),
            seg(3, 200, 300, "c"),
            seg(4, 300, 400, "d"),
        ];
        let pairs = vec![(1, 2), (2, 3)];
        let result = apply_merges(&segments, &pairs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "a b c");
        assert_eq!(result[1].text, "d");
    }

    #[test]
    fn apply_merges_no_pairs() {
        let segments = vec![seg(1, 0, 100, "a"), seg(2, 100, 200, "b")];
        let result = apply_merges(&segments, &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn build_chunks_short_input() {
        let segments: Vec<_> = (1..=5).map(|i| seg(i, 0, 100, "x")).collect();
        let chunks = build_chunks(&segments);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 5);
    }

    #[test]
    fn build_chunks_overlap() {
        let segments: Vec<_> = (1..=45).map(|i| seg(i, 0, 100, "x")).collect();
        let chunks = build_chunks(&segments);
        assert_eq!(chunks.len(), 3);
        // First chunk fully filled
        assert_eq!(chunks[0].len(), 20);
        // Adjacent chunks share OVERLAP=1 segment
        assert_eq!(
            chunks[0].last().unwrap().index,
            chunks[1].first().unwrap().index
        );
    }
}
