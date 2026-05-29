//! Translation pipeline as discrete, named stages.
//!
//! The LLM pipeline is broken down into stages so that:
//! - Each stage has a single responsibility (no 130-line function)
//! - Adding a new stage (e.g. reflexion, chain-of-thought) is mechanical
//! - The orchestrator in mod.rs reads top-down like a recipe
//!
//! State flows through `PipelineState`, mutated by each stage.

use std::path::Path;

use super::{quality, tm};
use crate::config::Config;
use crate::util::Segment;

/// Mutable state passed between pipeline stages.
pub struct PipelineState<'a> {
    /// Source texts (1:1 with segments)
    pub texts: &'a [String],
    /// Segments (for duration info)
    pub segments: &'a [Segment],
    /// Translations being produced (filled in by translate stage)
    pub results: Vec<String>,
    /// Per-cue quality score; -1.0 means "not scored"
    pub scores: Vec<f64>,
    /// All terms (existing TM + newly extracted)
    pub all_terms: Vec<quality::TermEntry>,
    /// Formatted terminology prompt
    pub terms_prompt: String,
    /// Loaded TM memory entries (for in-prompt retrieval)
    pub memory: Vec<tm::MemoryEntry>,
    /// TM directory (where to persist glossary/memory)
    pub tm_dir: Option<std::path::PathBuf>,
}

impl<'a> PipelineState<'a> {
    pub fn new(
        texts: &'a [String],
        segments: &'a [Segment],
        context_dir: Option<&Path>,
        cfg: &Config,
    ) -> Self {
        // Resolve TM directory:
        // - If cfg.tm_dir is set, use it (allows shared TM across videos)
        // - Otherwise, create .subforge-tm/ in the input file's parent directory
        let tm_dir = if !cfg.tm_dir.is_empty() {
            Some(tm::ensure_tm_dir_at(Path::new(&cfg.tm_dir)))
        } else {
            context_dir.map(tm::ensure_tm_dir)
        };
        Self {
            texts,
            segments,
            results: vec![String::new(); texts.len()],
            scores: vec![SCORE_UNKNOWN; texts.len()],
            all_terms: Vec::new(),
            terms_prompt: String::new(),
            memory: Vec::new(),
            tm_dir,
        }
    }
}

/// Sentinel value: GEMBA didn't successfully score this entry.
/// Distinguished from any valid 0..=100 score so we can skip it in TM save/refine.
pub const SCORE_UNKNOWN: f64 = -1.0;

/// Stage 1: Load existing glossary, extract new keywords (MAPS), merge.
pub async fn stage_extract_terminology(state: &mut PipelineState<'_>, cfg: &Config) {
    let existing_terms = state
        .tm_dir
        .as_ref()
        .map(|d| quality::load_glossary(d))
        .unwrap_or_default();

    crate::log_info!("  extracting keywords...");
    let new_terms = quality::extract_keywords(
        state.texts,
        &cfg.target_language,
        &cfg.api_key,
        &cfg.base_url,
        &cfg.model,
    )
    .await
    .unwrap_or_default();

    let mut all = existing_terms;
    for t in &new_terms {
        if !all.iter().any(|e| e.term == t.term) {
            all.push(t.clone());
        }
    }
    state.terms_prompt = quality::format_terms_for_prompt(&all);
    state.all_terms = all;
    crate::log_info!(
        "  translating with terminology ({} terms)...",
        state.all_terms.len()
    );
}

/// Stage 2: Load translation memory for in-prompt retrieval.
pub fn stage_load_memory(state: &mut PipelineState<'_>) {
    state.memory = state
        .tm_dir
        .as_ref()
        .map(|d| tm::load_memory(d))
        .unwrap_or_default();
}

/// Stage 3: Run two-phase batched translation (defined in batch.rs).
pub async fn stage_translate(state: &mut PipelineState<'_>, cfg: &Config) -> Result<(), String> {
    state.results = super::batch::two_phase_translate(
        state.texts,
        state.segments,
        cfg,
        &state.terms_prompt,
        &state.memory,
    )
    .await?;
    Ok(())
}

/// Stage 4: GEMBA-MQM quality estimation.
/// On failure, marks affected indices with SCORE_UNKNOWN so refine and TM save skip them.
pub async fn stage_quality_estimation(state: &mut PipelineState<'_>, cfg: &Config) {
    if !cfg.quality_estimation {
        return;
    }
    crate::log_info!("  evaluating quality...");
    let gemba = quality::evaluate_gemba(
        state.texts,
        &state.results,
        &cfg.target_language,
        cfg.qe_api_key(),
        cfg.qe_base_url(),
        &cfg.qe_model_or_default(),
        cfg.safe_thread_num(),
    )
    .await;

    if !gemba.failed_indices.is_empty() {
        crate::log_warn!(
            "GEMBA failed for {}/{} segments — they will not be considered for refine",
            gemba.failed_indices.len(),
            state.texts.len()
        );
    }

    if gemba.scores.is_empty() {
        return;
    }

    for s in &gemba.scores {
        if s.index < state.scores.len() {
            state.scores[s.index] = s.score;
        }
    }
    let avg: f64 = gemba.scores.iter().map(|s| s.score).sum::<f64>() / gemba.scores.len() as f64;
    let low_count = gemba
        .scores
        .iter()
        .filter(|s| s.score < cfg.refine_threshold as f64)
        .count();
    crate::log_info!(
        "  quality: avg={avg:.0}, low={low_count}/{} (failed={})",
        gemba.scores.len(),
        gemba.failed_indices.len()
    );

    // Refine inline so it sees the freshly-collected scores. Only attempt
    // refine on segments that we successfully scored as low — failed ones stay as-is.
    if cfg.refine && low_count > 0 {
        crate::log_info!("  refining {low_count} low-score segments...");
        let refined = quality::refine_low_scores(
            state.texts,
            &mut state.results,
            &gemba.scores,
            cfg.refine_threshold as f64,
            &cfg.target_language,
            &state.terms_prompt,
            &cfg.api_key,
            &cfg.base_url,
            &cfg.model,
            cfg.safe_thread_num(),
        )
        .await;
        crate::log_info!("  refined {refined} segments");
    }

    // Failed indices stay at SCORE_UNKNOWN (already initialized that way).
}

/// Stage 5: Persist glossary + memory to .subforge-tm/.
///
/// Behavior depends on whether GEMBA-MQM was run:
/// - When QE ran: only entries with a successfully scored translation are
///   persisted. Failed-to-score entries stay out of TM (avoid polluting
///   future retrieval with un-vetted output).
/// - When QE was skipped (`quality_estimation = false`): the user opted out
///   of scoring, so we save everything with a sentinel score so TM still
///   accumulates. Without this, turning off QE would silently disable TM
///   entirely — surprising and undocumented behavior in the previous build.
pub fn stage_save_tm(state: &PipelineState<'_>, cfg: &Config) {
    let Some(tm_dir) = state.tm_dir.as_ref() else {
        return;
    };
    quality::save_glossary(&state.all_terms, tm_dir);

    let qe_enabled = cfg.quality_estimation;
    /// Sentinel score used when QE is disabled and we save everything anyway.
    /// Distinguishable from real GEMBA scores (which are 0..=100); chosen
    /// near the typical "good" floor so retrieval doesn't deprioritize it.
    const QE_OFF_SCORE: f64 = 80.0;

    let entries: Vec<tm::MemoryEntry> = state
        .texts
        .iter()
        .zip(state.results.iter())
        .enumerate()
        .filter(|(_, (s, t))| s.split_whitespace().count() >= 3 && !t.is_empty())
        .filter_map(|(i, (s, t))| {
            let score = state.scores.get(i).copied().unwrap_or(SCORE_UNKNOWN);
            if score >= 0.0 {
                // GEMBA verified — record the real score.
                Some(tm::MemoryEntry {
                    source: s.clone(),
                    target: t.clone(),
                    score,
                })
            } else if !qe_enabled {
                // QE disabled by user — fall back to sentinel score so TM
                // still accumulates instead of silently emptying out.
                Some(tm::MemoryEntry {
                    source: s.clone(),
                    target: t.clone(),
                    score: QE_OFF_SCORE,
                })
            } else {
                // QE was on but failed for this entry — exclude.
                None
            }
        })
        .collect();

    if let Err(e) = tm::save_memory(tm_dir, &entries) {
        crate::log_warn!("TM save failed: {e}");
    }
}
