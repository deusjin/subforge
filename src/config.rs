use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // ASR
    pub asr: String,
    pub whisper_model: String,
    pub whisper_language: String,
    pub whisper_device: String,
    /// Physical CUDA GPU index selected by the user. Empty means "not chosen
    /// yet"; runtime may prompt when multiple GPUs are detected.
    pub cuda_gpu: String,
    pub whisper_compute_type: String,
    pub whisper_api_model: String, // model name for whisper-api backend
    pub whisper_cpp_model: String, // path/name for whisper.cpp backend
    pub bijian_base_url: String,

    // Segmentation
    pub segmenter: String,
    pub max_chars_per_cue: u16,
    pub target_chars_per_cue: u16,
    pub restore_punctuation: bool,
    pub polish_with_llm: bool,

    // Translation strategy
    /// When true (default): batches run sequentially after warm-up, each seeing
    /// the previous batch's translations as context (true moving window, best quality).
    /// When false: batches run concurrently with frozen context from warm-up
    /// (~2-3x faster on long videos, slightly worse continuity).
    pub chained_translation: bool,

    // Subtitle layout
    pub layout: String,
    /// Per-line color for the translation line (RRGGBB hex). When set,
    /// `subforge subtitle` injects ASS override tags (`{\c&Hbbggrr&}...{\r}`)
    /// around the translation line in the output SRT. libass interprets these
    /// during hard burn; modern players (mpv, VLC, MPC-HC) interpret them in
    /// soft-mux too. Empty = no color override (libass default = white).
    pub target_color: String,
    /// Per-line color for the original (source) line. Same semantics as
    /// `target_color`.
    pub source_color: String,

    // Translation
    pub translator: String,
    pub target_language: String,
    pub thread_num: u16,
    pub batch_size: u16,

    // LLM (default: shared by translation, polish, refine, QE).
    pub api_key: String,
    pub base_url: String,
    pub model: String,

    // Per-task LLM endpoint overrides. Empty → fall back to the shared
    // `api_key`/`base_url` above. Use these to e.g. route ASR transcription
    // at OpenAI Whisper while keeping translation on a local LLM.
    pub asr_api_key: String,
    pub asr_base_url: String,
    pub qe_api_key: String,
    pub qe_base_url: String,
    pub polish_api_key: String,
    pub polish_base_url: String,
    pub polish_model: String,

    // Quality pipeline
    pub quality_estimation: bool,
    pub qe_model: String,
    pub refine: bool,
    pub refine_threshold: u16,

    // Storage
    pub data_dir: String,
    /// Optional: explicit translation memory directory. If empty, TM is created
    /// in the input file's parent directory (recommended). Set this to share
    /// TM across multiple videos in different directories.
    pub tm_dir: String,

    /// Optional override for the HuggingFace org used by `subforge model
    /// download`. Defaults to "Systran" (faster-whisper's official mirror).
    /// Set to a fork or mirror if upstream is unavailable.
    pub hf_model_repo_prefix: String,

    // === Subtitle synthesis defaults ===
    //
    // CLI flags (`subforge synthesize --xxx ...` / `subforge process
    // --xxx ...`) override these. Empty string / 0 means "not set, use
    // built-in default". For numeric fields with a meaningful 0 (e.g.
    // CRF=0 is lossless), the CLI flag is the way to opt in explicitly.
    pub synth_mode: String,
    pub synth_font: String,
    pub synth_font_size: u32,
    pub synth_font_color: String,
    pub synth_outline_color: String,
    pub synth_outline_width: u32,
    pub synth_position: String,
    pub synth_margin_v: u32,
    pub synth_encoder: String,
    pub synth_crf: u8,
    pub synth_preset: String,
    pub synth_max_bitrate: String,

    /// Force the subtitle to occupy at most N% of the video width (1-100).
    /// Internally this is converted to libass `MarginL`/`MarginR` so each
    /// side margin = `(100 - N) * PlayResX / 200`. libass's PlayResX
    /// for SRT is 384, so the resulting script-unit margin is independent
    /// of the actual video resolution. Setting 90 means subtitles can use
    /// 90% of the screen width before wrapping (≈19 unit margins each side).
    /// 0 = unset, use libass default (~10 unit margins, ~95% width but with
    /// surprisingly different behavior because of font/wrap interaction).
    pub synth_width_ratio: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            asr: "bijian".into(),
            whisper_model: "base".into(),
            whisper_language: String::new(),
            whisper_device: "auto".into(),
            cuda_gpu: String::new(),
            whisper_compute_type: "auto".into(),
            whisper_api_model: "whisper-1".into(),
            whisper_cpp_model: "models/ggml-base.bin".into(),
            bijian_base_url: "https://member.bilibili.com/x/bcut/rubick-interface".into(),

            segmenter: "sat".into(),
            max_chars_per_cue: 100,
            target_chars_per_cue: 60,
            restore_punctuation: false,
            polish_with_llm: false,

            chained_translation: true,

            layout: "target-above".into(),
            target_color: String::new(),
            source_color: String::new(),

            translator: "google".into(),
            target_language: "zh-Hans".into(),
            thread_num: 3,
            batch_size: 7,

            api_key: String::new(),
            base_url: String::new(),
            model: "gpt-4o-mini".into(),

            asr_api_key: String::new(),
            asr_base_url: String::new(),
            qe_api_key: String::new(),
            qe_base_url: String::new(),
            polish_api_key: String::new(),
            polish_base_url: String::new(),
            polish_model: String::new(),

            quality_estimation: true,
            qe_model: String::new(),
            refine: true,
            refine_threshold: 70,

            data_dir: ".subforge".into(),
            tm_dir: String::new(),

            hf_model_repo_prefix: String::new(),

            // Synthesis defaults — all unset, will fall back to built-in
            // (Mode::Hard / libx264 / no force_style) at the call site.
            synth_mode: String::new(),
            synth_font: String::new(),
            synth_font_size: 0,
            synth_font_color: String::new(),
            synth_outline_color: String::new(),
            synth_outline_width: 0,
            synth_position: String::new(),
            synth_margin_v: 0,
            synth_encoder: String::new(),
            synth_crf: 0,
            synth_preset: String::new(),
            synth_max_bitrate: String::new(),
            synth_width_ratio: 0,
        }
    }
}

/// Discover the config file path with XDG-style fallback.
///
/// Resolution order (first hit wins):
///   1. `$SUBFORGE_CONFIG` environment variable
///   2. `<cwd>/config.toml` (per-project — main use case)
///   3. `$XDG_CONFIG_HOME/subforge/config.toml` (Linux/macOS)
///   4. `$HOME/.config/subforge/config.toml`
///   5. `<cwd>/config.toml` (synthesized; may not exist yet)
///
/// Falling back to the cwd path keeps the legacy behavior so a fresh user in
/// a project directory still sees the per-project config first.
pub fn resolve_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SUBFORGE_CONFIG") {
        return PathBuf::from(p);
    }
    let cwd_cfg = std::env::current_dir()
        .unwrap_or_default()
        .join("config.toml");
    if cwd_cfg.exists() {
        return cwd_cfg;
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let p = PathBuf::from(xdg).join("subforge").join("config.toml");
        if p.exists() {
            return p;
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home)
            .join(".config")
            .join("subforge")
            .join("config.toml");
        if p.exists() {
            return p;
        }
    }
    // Windows: %USERPROFILE%\.config\subforge\config.toml — keeps the same
    // dotfile layout users may already have on a Linux machine, while
    // still working when neither HOME nor XDG_CONFIG_HOME is set.
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(profile)
            .join(".config")
            .join("subforge")
            .join("config.toml");
        if p.exists() {
            return p;
        }
    }
    cwd_cfg
}

impl Config {
    pub fn load(path: &Path) -> Self {
        let mut cfg: Config = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            cfg.api_key = key;
        }
        if let Ok(url) = std::env::var("OPENAI_BASE_URL") {
            cfg.base_url = url;
        }
        cfg
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.to_pretty_toml()).map_err(|e| e.to_string())
    }

    // === Path resolution (single source of truth) ===

    pub fn data_path(&self) -> PathBuf {
        PathBuf::from(&self.data_dir)
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.data_path().join("cache")
    }
    pub fn tools_dir(&self) -> PathBuf {
        self.data_path().join("tools")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.data_path().join("models")
    }

    /// Path to the shared Python venv that holds faster-whisper, wtpsplit, suber, torch.
    /// The directory is named for historical reasons; the venv hosts all Python sidecars.
    pub fn venv_dir(&self) -> PathBuf {
        self.tools_dir().join("faster-whisper-cli").join("venv")
    }

    /// Path to a binary inside the venv (e.g. `python`, `suber`, `faster-whisper`).
    ///
    /// Cross-platform: Python venvs put binaries under `bin/` on Unix and
    /// `Scripts/` on Windows, with `.exe` extensions on Windows. Both are
    /// tried in order so the same `cfg.venv_bin("python")` call works on
    /// any platform without callers having to special-case.
    pub fn venv_bin(&self, name: &str) -> PathBuf {
        let venv = self.venv_dir();

        // On Windows, try Scripts\name.exe first (most common venv layout)
        // and Scripts\name as a fallback for binaries shipped without .exe.
        // On Unix only bin/name is meaningful.
        #[cfg(windows)]
        {
            let win_exe = venv.join("Scripts").join(format!("{name}.exe"));
            if win_exe.exists() {
                return win_exe;
            }
            let win_plain = venv.join("Scripts").join(name);
            if win_plain.exists() {
                return win_plain;
            }
        }

        venv.join("bin").join(name)
    }

    /// Resolve the python interpreter, falling back to system python.
    /// On Windows the fallback is `python` (not `python3` — Microsoft Store
    /// Python and python.org installers ship `python.exe`).
    pub fn python_path(&self) -> String {
        let venv_python = self.venv_bin("python");
        if venv_python.exists() {
            return venv_python.to_string_lossy().into_owned();
        }
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    }

    /// Resolve a sidecar binary; tries venv first, then `./sidecars/`, then PATH.
    pub fn sidecar_path(&self, name: &str) -> String {
        let venv = self.venv_bin(name);
        if venv.exists() {
            return venv.to_string_lossy().into_owned();
        }
        let local = PathBuf::from("sidecars").join(name);
        if local.exists() {
            return local.to_string_lossy().into_owned();
        }
        name.to_string()
    }

    /// Local faster-whisper model directory if downloaded, else the bare model name
    /// (which faster-whisper will auto-download from HuggingFace).
    pub fn faster_whisper_model_arg(&self) -> String {
        let local = self
            .models_dir()
            .join("faster-whisper")
            .join(&self.whisper_model);
        if local.join("model.bin").exists() {
            local.to_string_lossy().into_owned()
        } else {
            self.whisper_model.clone()
        }
    }

    pub fn qe_model_or_default(&self) -> String {
        if self.qe_model.is_empty() {
            self.model.clone()
        } else {
            self.qe_model.clone()
        }
    }

    /// Concurrency floor: `Semaphore::new(0)` permits **never** become
    /// available, deadlocking every spawned task. If a stale config or
    /// hand-edited TOML left thread_num at 0, clamp to 1 at the use site
    /// so the pipeline runs (slowly) instead of hanging forever.
    pub fn safe_thread_num(&self) -> usize {
        self.thread_num.max(1) as usize
    }

    /// Same idea as `safe_thread_num`. batch_size=0 leads to `chunks(0)`
    /// which `Vec::chunks` panics on. Clamp to 1 to keep the pipeline alive.
    pub fn safe_batch_size(&self) -> usize {
        self.batch_size.max(1) as usize
    }
    /// API key to use for ASR (whisper-api). Falls back to the shared key.
    pub fn asr_api_key(&self) -> &str {
        if self.asr_api_key.is_empty() {
            &self.api_key
        } else {
            &self.asr_api_key
        }
    }

    /// Base URL to use for ASR (whisper-api). Falls back to the shared URL.
    pub fn asr_base_url(&self) -> &str {
        if self.asr_base_url.is_empty() {
            &self.base_url
        } else {
            &self.asr_base_url
        }
    }

    /// API key for GEMBA QE / refine. Falls back to the shared key.
    pub fn qe_api_key(&self) -> &str {
        if self.qe_api_key.is_empty() {
            &self.api_key
        } else {
            &self.qe_api_key
        }
    }

    /// Base URL for GEMBA QE / refine. Falls back to the shared URL.
    pub fn qe_base_url(&self) -> &str {
        if self.qe_base_url.is_empty() {
            &self.base_url
        } else {
            &self.qe_base_url
        }
    }

    /// API key for LLM segment polish. Falls back to the shared key.
    pub fn polish_api_key(&self) -> &str {
        if self.polish_api_key.is_empty() {
            &self.api_key
        } else {
            &self.polish_api_key
        }
    }

    /// Base URL for LLM segment polish. Falls back to the shared URL.
    pub fn polish_base_url(&self) -> &str {
        if self.polish_base_url.is_empty() {
            &self.base_url
        } else {
            &self.polish_base_url
        }
    }

    /// Model for LLM segment polish. Falls back to the shared model.
    pub fn polish_model_or_default(&self) -> String {
        if self.polish_model.is_empty() {
            self.model.clone()
        } else {
            self.polish_model.clone()
        }
    }

    // === Cache key builders ===
    //
    // These consolidate ALL config fields that affect a given pipeline step.
    // When we add new config that affects transcription or translation, we update
    // ONE place. The string format is human-readable and stable.

    pub fn cache_key_transcribe(&self) -> String {
        // Note: api_key is intentionally excluded — it's auth, not behavior.
        // base_url IS included since different endpoints may serve different models.
        format!(
            "asr={asr}|model={m}|lang={lang}|device={d}|cuda_gpu={gpu}|ct={ct}|api_model={am}|cpp_model={cm}|\
             bijian={bj}|base_url={bu}|asr_base_url={abu}|seg={s}|max={mx}|tgt={tg}|punct={rp}|\
             polish={pl}|polish_url={pu}|polish_model={pm}|hf_prefix={hp}",
            asr = self.asr,
            m = self.whisper_model,
            lang = self.whisper_language,
            d = self.whisper_device,
            gpu = self.cuda_gpu,
            ct = self.whisper_compute_type,
            am = self.whisper_api_model,
            cm = self.whisper_cpp_model,
            bj = self.bijian_base_url,
            bu = self.base_url,
            abu = self.asr_base_url,
            s = self.segmenter,
            mx = self.max_chars_per_cue,
            tg = self.target_chars_per_cue,
            rp = self.restore_punctuation,
            pl = self.polish_with_llm,
            pu = if self.polish_with_llm {
                self.polish_base_url()
            } else {
                ""
            },
            pm = if self.polish_with_llm {
                self.polish_model_or_default()
            } else {
                String::new()
            },
            // hf_model_repo_prefix routes `subforge model download` to a
            // different HF org. If a user switches prefix and re-downloads
            // a model with the same NAME (e.g. "small"), the bytes on disk
            // change. Include the prefix so we don't false-hit on a stale
            // transcription produced under the previous repo.
            hp = self.hf_model_repo_prefix,
        )
    }

    pub fn cache_key_subtitle(&self) -> String {
        // Note: api_key is intentionally excluded.
        format!(
            "translator={t}|lang={l}|layout={la}|tcol={tc}|scol={sc}|base_url={bu}|model={m}|\
             batch={b}|threads={th}|chained={ch}|qe={qe}|qe_model={qm}|qe_url={qu}|refine={r}|refine_th={rt}",
            t = self.translator,
            l = self.target_language,
            la = self.layout,
            tc = self.target_color,
            sc = self.source_color,
            bu = self.base_url,
            m = self.model,
            b = self.batch_size,
            th = self.thread_num,
            ch = self.chained_translation,
            qe = self.quality_estimation,
            qm = self.qe_model,
            qu = self.qe_base_url,
            r = self.refine,
            rt = self.refine_threshold,
        )
    }

    // === Generic get/set for config CLI ===

    pub fn get(&self, key: &str) -> Option<String> {
        match key {
            "asr" => Some(self.asr.clone()),
            "translator" => Some(self.translator.clone()),
            "target_language" => Some(self.target_language.clone()),
            "layout" => Some(self.layout.clone()),
            "target_color" => Some(self.target_color.clone()),
            "source_color" => Some(self.source_color.clone()),
            "thread_num" => Some(self.thread_num.to_string()),
            "batch_size" => Some(self.batch_size.to_string()),
            "api_key" => Some(redact_secret(&self.api_key)),
            "base_url" => Some(self.base_url.clone()),
            "model" => Some(self.model.clone()),
            "asr_api_key" => Some(redact_secret(&self.asr_api_key)),
            "asr_base_url" => Some(self.asr_base_url.clone()),
            "qe_api_key" => Some(redact_secret(&self.qe_api_key)),
            "qe_base_url" => Some(self.qe_base_url.clone()),
            "polish_api_key" => Some(redact_secret(&self.polish_api_key)),
            "polish_base_url" => Some(self.polish_base_url.clone()),
            "polish_model" => Some(if self.polish_model.is_empty() {
                format!("(=model: {})", self.model)
            } else {
                self.polish_model.clone()
            }),
            "bijian_base_url" => Some(self.bijian_base_url.clone()),
            "whisper_model" => Some(self.whisper_model.clone()),
            "whisper_language" => Some(self.whisper_language.clone()),
            "whisper_device" => Some(self.whisper_device.clone()),
            "cuda_gpu" => Some(if self.cuda_gpu.is_empty() {
                "(未指定)".into()
            } else {
                self.cuda_gpu.clone()
            }),
            "whisper_compute_type" => Some(self.whisper_compute_type.clone()),
            "whisper_api_model" => Some(self.whisper_api_model.clone()),
            "whisper_cpp_model" => Some(self.whisper_cpp_model.clone()),
            "data_dir" => Some(self.data_dir.clone()),
            "tm_dir" => Some(if self.tm_dir.is_empty() {
                "(自动：视频所在目录)".into()
            } else {
                self.tm_dir.clone()
            }),
            "hf_model_repo_prefix" => Some(if self.hf_model_repo_prefix.is_empty() {
                "(默认: Systran)".into()
            } else {
                self.hf_model_repo_prefix.clone()
            }),
            "synth_mode" => Some(if self.synth_mode.is_empty() {
                "(默认: hard)".into()
            } else {
                self.synth_mode.clone()
            }),
            "synth_font" => Some(self.synth_font.clone()),
            "synth_font_size" => Some(if self.synth_font_size == 0 {
                "(libass 默认)".into()
            } else {
                self.synth_font_size.to_string()
            }),
            "synth_font_color" => Some(self.synth_font_color.clone()),
            "synth_outline_color" => Some(self.synth_outline_color.clone()),
            "synth_outline_width" => Some(if self.synth_outline_width == 0 {
                "(libass 默认)".into()
            } else {
                self.synth_outline_width.to_string()
            }),
            "synth_position" => Some(if self.synth_position.is_empty() {
                "(默认: bottom)".into()
            } else {
                self.synth_position.clone()
            }),
            "synth_margin_v" => Some(if self.synth_margin_v == 0 {
                "(libass 默认)".into()
            } else {
                self.synth_margin_v.to_string()
            }),
            "synth_encoder" => Some(if self.synth_encoder.is_empty() {
                "(默认: x264)".into()
            } else {
                self.synth_encoder.clone()
            }),
            "synth_crf" => Some(if self.synth_crf == 0 {
                "(编码器默认)".into()
            } else {
                self.synth_crf.to_string()
            }),
            "synth_preset" => Some(if self.synth_preset.is_empty() {
                "(编码器默认)".into()
            } else {
                self.synth_preset.clone()
            }),
            "synth_max_bitrate" => Some(self.synth_max_bitrate.clone()),
            "synth_width_ratio" => Some(if self.synth_width_ratio == 0 {
                "(libass 默认)".into()
            } else {
                format!("{}%", self.synth_width_ratio)
            }),
            "segmenter" => Some(self.segmenter.clone()),
            "max_chars_per_cue" => Some(self.max_chars_per_cue.to_string()),
            "target_chars_per_cue" => Some(self.target_chars_per_cue.to_string()),
            "restore_punctuation" => Some(self.restore_punctuation.to_string()),
            "polish_with_llm" => Some(self.polish_with_llm.to_string()),
            "chained_translation" => Some(self.chained_translation.to_string()),
            "quality_estimation" => Some(self.quality_estimation.to_string()),
            "qe_model" => Some(if self.qe_model.is_empty() {
                format!("(=model: {})", self.model)
            } else {
                self.qe_model.clone()
            }),
            "refine" => Some(self.refine.to_string()),
            "refine_threshold" => Some(self.refine_threshold.to_string()),
            _ => None,
        }
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "asr" => {
                if !["bijian", "faster-whisper", "whisper-api", "whisper-cpp"].contains(&value) {
                    return Err(format!("无效值: {value}"));
                }
                self.asr = value.into();
                Ok(())
            }
            "translator" => {
                if !["google", "bing", "llm", ""].contains(&value) {
                    return Err(format!("无效值: {value}"));
                }
                self.translator = value.into();
                Ok(())
            }
            "target_language" => {
                self.target_language = value.into();
                Ok(())
            }
            "layout" => {
                if !["target-above", "source-above", "target-only", "source-only"].contains(&value)
                {
                    return Err(format!("无效值: {value}"));
                }
                self.layout = value.into();
                Ok(())
            }
            "target_color" => {
                if !value.is_empty() && !is_rrggbb(value) {
                    return Err("需要 RRGGBB hex 格式（例如 FFFF00）或留空".into());
                }
                self.target_color = value.into();
                Ok(())
            }
            "source_color" => {
                if !value.is_empty() && !is_rrggbb(value) {
                    return Err("需要 RRGGBB hex 格式（例如 FFFFFF）或留空".into());
                }
                self.source_color = value.into();
                Ok(())
            }
            "thread_num" => {
                let n: u16 = value.parse().map_err(|_| "需要正整数".to_string())?;
                if n == 0 {
                    return Err("thread_num 必须 ≥ 1（0 会导致并发死锁）".into());
                }
                self.thread_num = n;
                Ok(())
            }
            "batch_size" => {
                let n: u16 = value.parse().map_err(|_| "需要正整数".to_string())?;
                if n == 0 {
                    return Err("batch_size 必须 ≥ 1".into());
                }
                self.batch_size = n;
                Ok(())
            }
            "api_key" => {
                self.api_key = value.into();
                Ok(())
            }
            "base_url" => {
                self.base_url = value.into();
                Ok(())
            }
            "model" => {
                self.model = value.into();
                Ok(())
            }
            "asr_api_key" => {
                self.asr_api_key = value.into();
                Ok(())
            }
            "asr_base_url" => {
                self.asr_base_url = value.into();
                Ok(())
            }
            "qe_api_key" => {
                self.qe_api_key = value.into();
                Ok(())
            }
            "qe_base_url" => {
                self.qe_base_url = value.into();
                Ok(())
            }
            "polish_api_key" => {
                self.polish_api_key = value.into();
                Ok(())
            }
            "polish_base_url" => {
                self.polish_base_url = value.into();
                Ok(())
            }
            "polish_model" => {
                self.polish_model = value.into();
                Ok(())
            }
            "bijian_base_url" => {
                self.bijian_base_url = value.into();
                Ok(())
            }
            "whisper_model" => {
                self.whisper_model = value.into();
                Ok(())
            }
            "whisper_language" => {
                self.whisper_language = value.into();
                Ok(())
            }
            "whisper_device" => {
                if !["auto", "cuda", "cpu"].contains(&value) {
                    return Err("需要 auto / cuda / cpu".into());
                }
                self.whisper_device = value.into();
                Ok(())
            }
            "cuda_gpu" => {
                if !value.is_empty() {
                    value
                        .parse::<u32>()
                        .map_err(|_| "需要 GPU 编号（如 0 / 1）或留空".to_string())?;
                }
                self.cuda_gpu = value.into();
                Ok(())
            }
            "whisper_compute_type" => {
                if ![
                    "auto",
                    "float16",
                    "float32",
                    "int8",
                    "int8_float16",
                    "int8_float32",
                ]
                .contains(&value)
                {
                    return Err(
                        "需要 auto / float16 / float32 / int8 / int8_float16 / int8_float32".into(),
                    );
                }
                self.whisper_compute_type = value.into();
                Ok(())
            }
            "whisper_api_model" => {
                self.whisper_api_model = value.into();
                Ok(())
            }
            "whisper_cpp_model" => {
                self.whisper_cpp_model = value.into();
                Ok(())
            }
            "data_dir" => {
                self.data_dir = value.into();
                Ok(())
            }
            "tm_dir" => {
                self.tm_dir = value.into();
                Ok(())
            }
            "hf_model_repo_prefix" => {
                self.hf_model_repo_prefix = value.into();
                Ok(())
            }

            // Synthesis defaults — validate via the same parsers `subforge
            // synthesize` uses, so an invalid TOML value is caught here
            // rather than silently fed to ffmpeg.
            "synth_mode" => {
                if !value.is_empty() {
                    crate::synthesize::Mode::parse(value)?;
                }
                self.synth_mode = value.into();
                Ok(())
            }
            "synth_font" => {
                self.synth_font = value.into();
                Ok(())
            }
            "synth_font_size" => {
                self.synth_font_size = value.parse().map_err(|_| "需要非负整数".to_string())?;
                Ok(())
            }
            "synth_font_color" => {
                if !value.is_empty() && !is_rrggbb(value) {
                    return Err("需要 RRGGBB hex 格式（例如 FFFFFF）".into());
                }
                self.synth_font_color = value.into();
                Ok(())
            }
            "synth_outline_color" => {
                if !value.is_empty() && !is_rrggbb(value) {
                    return Err("需要 RRGGBB hex 格式（例如 000000）".into());
                }
                self.synth_outline_color = value.into();
                Ok(())
            }
            "synth_outline_width" => {
                self.synth_outline_width = value.parse().map_err(|_| "需要非负整数".to_string())?;
                Ok(())
            }
            "synth_position" => {
                if !value.is_empty() {
                    crate::synthesize::Position::parse(value)?;
                }
                self.synth_position = value.into();
                Ok(())
            }
            "synth_margin_v" => {
                self.synth_margin_v = value.parse().map_err(|_| "需要非负整数".to_string())?;
                Ok(())
            }
            "synth_encoder" => {
                if !value.is_empty() {
                    crate::synthesize::Encoder::parse(value)?;
                }
                self.synth_encoder = value.into();
                Ok(())
            }
            "synth_crf" => {
                let n: u8 = value.parse().map_err(|_| "需要 0-51 整数".to_string())?;
                if n > 51 {
                    return Err(format!("synth_crf 必须在 0-51，得到 {n}"));
                }
                self.synth_crf = n;
                Ok(())
            }
            "synth_preset" => {
                self.synth_preset = value.into();
                Ok(())
            }
            "synth_max_bitrate" => {
                self.synth_max_bitrate = value.into();
                Ok(())
            }
            "synth_width_ratio" => {
                let n: u8 = value
                    .parse()
                    .map_err(|_| "需要 0-100 整数（百分比）".to_string())?;
                if n > 100 {
                    return Err(format!("synth_width_ratio 必须在 0-100，得到 {n}"));
                }
                self.synth_width_ratio = n;
                Ok(())
            }
            "segmenter" => {
                if !["sat", "rule"].contains(&value) {
                    return Err("需要 sat 或 rule".into());
                }
                self.segmenter = value.into();
                Ok(())
            }
            "max_chars_per_cue" => {
                self.max_chars_per_cue = value.parse().map_err(|_| "需要正整数".to_string())?;
                Ok(())
            }
            "target_chars_per_cue" => {
                self.target_chars_per_cue = value.parse().map_err(|_| "需要正整数".to_string())?;
                Ok(())
            }
            "restore_punctuation" => {
                self.restore_punctuation =
                    value.parse().map_err(|_| "需要 true/false".to_string())?;
                Ok(())
            }
            "polish_with_llm" => {
                self.polish_with_llm = value.parse().map_err(|_| "需要 true/false".to_string())?;
                Ok(())
            }
            "chained_translation" => {
                self.chained_translation =
                    value.parse().map_err(|_| "需要 true/false".to_string())?;
                Ok(())
            }
            "quality_estimation" => {
                self.quality_estimation =
                    value.parse().map_err(|_| "需要 true/false".to_string())?;
                Ok(())
            }
            "qe_model" => {
                self.qe_model = value.into();
                Ok(())
            }
            "refine" => {
                self.refine = value.parse().map_err(|_| "需要 true/false".to_string())?;
                Ok(())
            }
            "refine_threshold" => {
                self.refine_threshold = value.parse().map_err(|_| "需要 0-100 整数".to_string())?;
                Ok(())
            }
            _ => Err(format!("未知配置项: {key}")),
        }
    }

    /// Generate a human-readable TOML with sections and inline comments
    fn to_pretty_toml(&self) -> String {
        let mut out = String::new();
        out.push_str("# ============================================================\n");
        out.push_str("# SUBFORGE Configuration\n");
        out.push_str("# 用 `subforge config set <key> <value>` 修改\n");
        out.push_str("# ============================================================\n\n");

        out.push_str("# --- ASR / 语音转录 ---\n");
        out.push_str(&format!(
            "asr                  = {:<22} # bijian / faster-whisper / whisper-api / whisper-cpp\n",
            q(&self.asr)
        ));
        out.push_str(&format!("whisper_model        = {:<22} # tiny / base / small / medium / large / turbo (.en 仅英语)\n", q(&self.whisper_model)));
        out.push_str(&format!(
            "whisper_language     = {:<22} # 留空自动检测，或 en / zh / ja / ko ...\n",
            q(&self.whisper_language)
        ));
        out.push_str(&format!(
            "whisper_device       = {:<22} # auto / cuda / cpu；auto 会在可见 CUDA 设备中自动选择\n",
            q(&self.whisper_device)
        ));
        out.push_str(&format!(
            "cuda_gpu             = {:<22} # 默认 CUDA GPU 编号；供 faster-whisper/NVENC 使用。留空时多 GPU 会提示选择\n",
            q(&self.cuda_gpu)
        ));
        out.push_str(&format!(
            "whisper_compute_type = {:<22} # auto / float16 / float32 / int8 / int8_float16\n",
            q(&self.whisper_compute_type)
        ));
        out.push_str(&format!(
            "whisper_api_model    = {:<22} # whisper-api 后端模型名\n",
            q(&self.whisper_api_model)
        ));
        out.push_str(&format!(
            "whisper_cpp_model    = {:<22} # whisper-cpp 后端模型路径\n",
            q(&self.whisper_cpp_model)
        ));
        out.push('\n');

        out.push_str("# --- 字幕分段 (SaT, EMNLP 2024) ---\n");
        out.push_str(&format!(
            "segmenter            = {:<22} # sat (神经网络) / rule (规则)\n",
            q(&self.segmenter)
        ));
        out.push_str(&format!(
            "max_chars_per_cue    = {:<22} # 单段最大字符数 (建议 80-140)\n",
            self.max_chars_per_cue
        ));
        out.push_str(&format!(
            "target_chars_per_cue = {:<22} # 偏好字符数\n",
            self.target_chars_per_cue
        ));
        out.push_str(&format!(
            "restore_punctuation  = {:<22} # BERT 标点恢复 (仅英文，慢)\n",
            self.restore_punctuation
        ));
        out.push_str(&format!(
            "polish_with_llm      = {:<22} # LLM 边界润色 (额外 API 调用)\n",
            self.polish_with_llm
        ));
        out.push_str(&format!("chained_translation  = {:<22} # true=顺序翻译，moving window 真实工作；false=并发，滑动窗口缓存\n", self.chained_translation));
        out.push('\n');

        out.push_str("# --- 字幕样式 ---\n");
        out.push_str(&format!("layout               = {:<22} # target-above / source-above / target-only / source-only\n", q(&self.layout)));
        out.push_str(&format!("target_color         = {:<22} # 译文行颜色 RRGGBB hex（如 FFFF00 黄色），留空 = libass 默认\n", q(&self.target_color)));
        out.push_str(&format!(
            "source_color         = {:<22} # 原文行颜色 RRGGBB hex，留空 = libass 默认\n",
            q(&self.source_color)
        ));
        out.push('\n');

        out.push_str("# --- 翻译 ---\n");
        out.push_str(&format!(
            "translator           = {:<22} # llm / google / bing / \"\" (不翻译)\n",
            q(&self.translator)
        ));
        out.push_str(&format!(
            "target_language      = {:<22} # zh-Hans / en / ja / ko / fr / de ...\n",
            q(&self.target_language)
        ));
        out.push_str(&format!(
            "thread_num           = {:<22} # 并发数 (1-10)\n",
            self.thread_num
        ));
        out.push_str(&format!(
            "batch_size           = {:<22} # LLM 每批字幕数 (3-20)\n",
            self.batch_size
        ));
        out.push('\n');

        out.push_str("# --- LLM 提供商 ---\n");
        out.push_str(&format!(
            "api_key              = {:<22} # 环境变量 OPENAI_API_KEY 优先\n",
            q(&self.api_key)
        ));
        out.push_str(&format!(
            "base_url             = {:<22} # 如 https://api.openai.com/v1\n",
            q(&self.base_url)
        ));
        out.push_str(&format!(
            "model                = {:<22} # 主翻译模型\n",
            q(&self.model)
        ));
        out.push('\n');

        out.push_str("# --- 可选：分别为 ASR / QE 指定不同的 LLM 端点 ---\n");
        out.push_str(&format!(
            "asr_api_key          = {:<22} # 留空时回退 api_key\n",
            q(&self.asr_api_key)
        ));
        out.push_str(&format!(
            "asr_base_url         = {:<22} # 留空时回退 base_url\n",
            q(&self.asr_base_url)
        ));
        out.push_str(&format!(
            "qe_api_key           = {:<22} # GEMBA / refine 用\n",
            q(&self.qe_api_key)
        ));
        out.push_str(&format!(
            "qe_base_url          = {:<22}\n",
            q(&self.qe_base_url)
        ));
        out.push('\n');

        out.push_str("# --- 翻译质量 (MAPS + GEMBA-MQM + Targeted Refine) ---\n");
        out.push_str(&format!(
            "quality_estimation   = {:<22} # GEMBA 质量评分\n",
            self.quality_estimation
        ));
        out.push_str(&format!(
            "qe_model             = {:<22} # QE 模型 (留空则用主 model)\n",
            q(&self.qe_model)
        ));
        out.push_str(&format!(
            "refine               = {:<22} # 低分自动重翻\n",
            self.refine
        ));
        out.push_str(&format!(
            "refine_threshold     = {:<22} # 重翻阈值 (0-100)\n",
            self.refine_threshold
        ));
        out.push('\n');

        out.push_str("# --- 其他 ---\n");
        out.push_str(&format!(
            "data_dir             = {:<22} # 缓存、模型、工具目录\n",
            q(&self.data_dir)
        ));
        out.push_str(&format!(
            "tm_dir               = {:<22} # 留空：视频所在目录；或显式指定共享路径\n",
            q(&self.tm_dir)
        ));
        out.push_str(&format!(
            "bijian_base_url      = {}\n",
            q(&self.bijian_base_url)
        ));
        out.push_str(&format!(
            "hf_model_repo_prefix = {:<22} # 留空使用 Systran\n",
            q(&self.hf_model_repo_prefix)
        ));
        out.push('\n');

        out.push_str("# --- 字幕烧制 / 软封装默认值 ---\n");
        out.push_str("# CLI flag (subforge synthesize / process) 优先于这里的值。\n");
        out.push_str("# 数值字段 0 = 未设置，使用编码器/libass 默认。\n");
        out.push_str(&format!("synth_mode           = {:<22} # hard / soft / both（留空 = hard；soft 默认 .mkv/SRT）\n", q(&self.synth_mode)));
        out.push_str(&format!(
            "synth_font           = {:<22} # 字体名（含空格用引号）\n",
            q(&self.synth_font)
        ));
        out.push_str(&format!(
            "synth_font_size      = {:<22} # 字号像素（0 = libass 默认）\n",
            self.synth_font_size
        ));
        out.push_str(&format!(
            "synth_font_color     = {:<22} # RRGGBB hex（如 FFFFFF）\n",
            q(&self.synth_font_color)
        ));
        out.push_str(&format!(
            "synth_outline_color  = {:<22} # RRGGBB hex（如 000000）\n",
            q(&self.synth_outline_color)
        ));
        out.push_str(&format!(
            "synth_outline_width  = {:<22} # 描边像素\n",
            self.synth_outline_width
        ));
        out.push_str(&format!(
            "synth_position       = {:<22} # bottom / top / center / bottom-right ...\n",
            q(&self.synth_position)
        ));
        out.push_str(&format!(
            "synth_margin_v       = {:<22} # 垂直边距像素\n",
            self.synth_margin_v
        ));
        out.push_str(&format!("synth_encoder        = {:<22} # x264 / x265 / nvenc / nvenc-hevc / qsv / videotoolbox\n", q(&self.synth_encoder)));
        out.push_str(&format!(
            "synth_crf            = {:<22} # 0-51 质量；0 = 编码器默认\n",
            self.synth_crf
        ));
        out.push_str(&format!(
            "synth_preset         = {:<22} # veryfast / fast / medium / slow / veryslow\n",
            q(&self.synth_preset)
        ));
        out.push_str(&format!(
            "synth_max_bitrate    = {:<22} # 如 8M / 5000k\n",
            q(&self.synth_max_bitrate)
        ));
        out.push_str(&format!("synth_width_ratio    = {:<22} # 字幕占据画面宽度的百分比 (1-100)；0 = libass 默认；推荐 90\n", self.synth_width_ratio));
        out.push('\n');

        out
    }
}

/// Show a credible portion of a secret: at most 4 prefix chars, then `...`.
/// Returns "(not set)" for empty, the full value for very-short strings (no
/// safe prefix to disclose). Previously, a 3-char key was returned in full
/// because the truncate logic assumed length ≥ 4.
fn redact_secret(s: &str) -> String {
    if s.is_empty() {
        return "(not set)".into();
    }
    if s.chars().count() <= 4 {
        return "***".into();
    }
    let prefix: String = s.chars().take(4).collect();
    format!("{prefix}...")
}

/// Cheap check for `RRGGBB` hex (case-insensitive, 6 chars, optional `#`).
fn is_rrggbb(s: &str) -> bool {
    let s = s.trim_start_matches('#');
    s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// (key, description, valid_options, show_in_list)
pub const CONFIG_HELP: &[(&str, &str, &str, bool)] = &[
    (
        "asr",
        "语音识别引擎",
        "bijian / faster-whisper / whisper-api / whisper-cpp",
        true,
    ),
    (
        "translator",
        "翻译器",
        "google / bing / llm / (空=不翻译)",
        true,
    ),
    (
        "target_language",
        "目标语言",
        "zh-Hans / en / ja / ko / fr / de ...",
        true,
    ),
    (
        "layout",
        "字幕布局",
        "target-above / source-above / target-only / source-only",
        true,
    ),
    (
        "target_color",
        "译文行颜色",
        "RRGGBB hex（如 FFFF00），留空 = libass 默认（白）",
        true,
    ),
    (
        "source_color",
        "原文行颜色",
        "RRGGBB hex，留空 = libass 默认",
        false,
    ),
    ("thread_num", "翻译并发数", "1-10", true),
    ("batch_size", "LLM每批条数", "3-20", true),
    (
        "api_key",
        "LLM API密钥",
        "环境变量 OPENAI_API_KEY 优先",
        true,
    ),
    (
        "base_url",
        "LLM API地址",
        "如 https://api.openai.com/v1",
        true,
    ),
    ("model", "LLM模型名", "如 gpt-4o-mini", true),
    ("asr_api_key", "ASR专用密钥", "留空回退到 api_key", false),
    ("asr_base_url", "ASR专用 URL", "留空回退到 base_url", false),
    ("qe_api_key", "QE 专用密钥", "留空回退到 api_key", false),
    ("qe_base_url", "QE 专用 URL", "留空回退到 base_url", false),
    (
        "polish_api_key",
        "polish 专用密钥",
        "留空回退到 api_key",
        false,
    ),
    (
        "polish_base_url",
        "polish 专用 URL",
        "留空回退到 base_url",
        false,
    ),
    ("polish_model", "polish 专用模型", "留空回退到 model", false),
    (
        "whisper_model",
        "Whisper模型",
        "tiny / base / small / medium / large / turbo",
        true,
    ),
    (
        "whisper_language",
        "Whisper语言",
        "留空自动检测，或 en / zh / ja ...",
        true,
    ),
    ("whisper_device", "Whisper设备", "auto / cuda / cpu", true),
    (
        "cuda_gpu",
        "默认 CUDA GPU",
        "供 faster-whisper/NVENC 使用；留空=首次多卡提示；可用 subforge gpu 设置",
        true,
    ),
    (
        "whisper_compute_type",
        "Whisper精度",
        "auto / float16 / float32 / int8",
        true,
    ),
    (
        "whisper_api_model",
        "whisper-api 模型",
        "whisper-1 / whisper-large-v3 / ...",
        true,
    ),
    (
        "whisper_cpp_model",
        "whisper-cpp 路径",
        "如 models/ggml-base.bin",
        true,
    ),
    (
        "segmenter",
        "分段器",
        "sat (SaT EMNLP'24) / rule (规则)",
        true,
    ),
    (
        "max_chars_per_cue",
        "字幕最大字符数",
        "30-200 (默认 100)",
        true,
    ),
    (
        "target_chars_per_cue",
        "字幕目标字符数",
        "20-100 (默认 60)",
        true,
    ),
    ("restore_punctuation", "标点恢复", "true / false", true),
    (
        "polish_with_llm",
        "LLM边界润色",
        "true / false (额外API调用)",
        true,
    ),
    (
        "chained_translation",
        "顺序翻译",
        "true=每批看上批结果（慢，质量好）/ false=并发+滑动窗口缓存",
        true,
    ),
    ("quality_estimation", "GEMBA质量评估", "true / false", true),
    ("qe_model", "QE评估模型", "留空则使用主模型", true),
    ("refine", "低分自动重翻", "true / false", true),
    ("refine_threshold", "重翻阈值", "0-100", true),
    ("bijian_base_url", "必剪API地址", "一般无需修改", false),
    ("data_dir", "数据目录", "缓存和模型存放位置", false),
    (
        "tm_dir",
        "翻译记忆目录",
        "留空：视频所在目录；或显式指定共享路径",
        false,
    ),
    (
        "hf_model_repo_prefix",
        "HF 模型仓库前缀",
        "留空使用 Systran/",
        false,
    ),
    // Synthesis defaults — show in `config show` so users discover them.
    (
        "synth_mode",
        "烧制模式默认",
        "hard / soft / both（留空 = hard）",
        true,
    ),
    ("synth_font", "字体名", "如 \"Source Han Sans\"", true),
    ("synth_font_size", "字号（像素）", "0 = libass 默认", true),
    ("synth_font_color", "字体颜色", "RRGGBB hex", true),
    ("synth_outline_color", "描边颜色", "RRGGBB hex", true),
    (
        "synth_outline_width",
        "描边宽度（像素）",
        "0 = libass 默认",
        false,
    ),
    (
        "synth_position",
        "字幕位置",
        "bottom / top / center / bottom-right ...",
        true,
    ),
    (
        "synth_margin_v",
        "垂直边距（像素）",
        "0 = libass 默认",
        false,
    ),
    (
        "synth_encoder",
        "视频编码器",
        "x264 / x265 / nvenc / nvenc-hevc / qsv / videotoolbox",
        true,
    ),
    ("synth_crf", "CRF 质量参数", "0-51；x264 推荐 18-28", true),
    (
        "synth_preset",
        "编码 preset",
        "veryfast / fast / medium / slow / veryslow",
        true,
    ),
    ("synth_max_bitrate", "最大码率", "如 8M / 5000k", false),
    (
        "synth_width_ratio",
        "字幕宽度比例(%)",
        "1-100，0=libass 默认；推荐 90",
        true,
    ),
];

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_transcribe_changes_with_segmenter() {
        let mut a = Config::default();
        let key1 = a.cache_key_transcribe();
        a.segmenter = "rule".into();
        let key2 = a.cache_key_transcribe();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_transcribe_changes_with_max_chars() {
        let mut a = Config::default();
        let key1 = a.cache_key_transcribe();
        a.max_chars_per_cue = 200;
        let key2 = a.cache_key_transcribe();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_subtitle_changes_with_threshold() {
        let mut a = Config::default();
        let key1 = a.cache_key_subtitle();
        a.refine_threshold = 80;
        let key2 = a.cache_key_subtitle();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_transcribe_changes_with_hf_prefix() {
        // Switching HF org for a model name like "small" produces different
        // bytes on disk despite the same model name. Cache must invalidate.
        let mut a = Config::default();
        let key1 = a.cache_key_transcribe();
        a.hf_model_repo_prefix = "BAAI".into();
        let key2 = a.cache_key_transcribe();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_transcribe_changes_with_cuda_gpu() {
        let mut a = Config::default();
        let key1 = a.cache_key_transcribe();
        a.cuda_gpu = "1".into();
        let key2 = a.cache_key_transcribe();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_subtitle_changes_with_target_color() {
        // target_color is rendered as an ASS override tag in the SRT.
        // Different color → different SRT bytes → cache must miss.
        let mut a = Config::default();
        let key1 = a.cache_key_subtitle();
        a.target_color = "FFFF00".into();
        let key2 = a.cache_key_subtitle();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_subtitle_changes_with_layout() {
        let mut a = Config::default();
        let key1 = a.cache_key_subtitle();
        a.layout = "target-only".into();
        let key2 = a.cache_key_subtitle();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_subtitle_changes_with_chained_translation() {
        // chained=true vs false produces different translation outputs.
        let mut a = Config::default();
        let key1 = a.cache_key_subtitle();
        a.chained_translation = !a.chained_translation;
        let key2 = a.cache_key_subtitle();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_transcribe_changes_with_polish_toggle() {
        let mut a = Config::default();
        let key1 = a.cache_key_transcribe();
        a.polish_with_llm = true;
        let key2 = a.cache_key_transcribe();
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_does_not_change_with_api_key() {
        // api_key is auth, not behavior. Rotating keys shouldn't bust cache.
        let mut a = Config {
            api_key: "key_v1".into(),
            ..Default::default()
        };
        let k1_t = a.cache_key_transcribe();
        let k1_s = a.cache_key_subtitle();
        a.api_key = "key_v2".into();
        let k2_t = a.cache_key_transcribe();
        let k2_s = a.cache_key_subtitle();
        assert_eq!(k1_t, k2_t);
        assert_eq!(k1_s, k2_s);
    }

    #[test]
    fn cache_key_changes_with_base_url() {
        // Different endpoint may serve different model, so base_url DOES matter.
        let mut a = Config {
            base_url: "https://api1/v1".into(),
            ..Default::default()
        };
        let k1 = a.cache_key_subtitle();
        a.base_url = "https://api2/v1".into();
        let k2 = a.cache_key_subtitle();
        assert_ne!(k1, k2);
    }

    #[test]
    fn paths_use_data_dir() {
        let cfg = Config {
            data_dir: "/tmp/foo".into(),
            ..Default::default()
        };
        assert_eq!(cfg.cache_dir(), PathBuf::from("/tmp/foo/cache"));
        assert_eq!(cfg.tools_dir(), PathBuf::from("/tmp/foo/tools"));
        assert_eq!(cfg.models_dir(), PathBuf::from("/tmp/foo/models"));
        // venv_bin: on Unix, falls through to bin/python. On Windows, since
        // the Scripts/python[.exe] paths don't exist for this fake data_dir,
        // it also falls through to the bin/python path.
        assert_eq!(
            cfg.venv_bin("python"),
            PathBuf::from("/tmp/foo/tools/faster-whisper-cli/venv/bin/python")
        );
    }

    #[test]
    fn python_path_falls_back_to_system_per_platform() {
        // Without an installed venv, we fall back to the system interpreter.
        // Different fallback name on Windows vs Unix because
        // python.org / Microsoft Store installers ship `python.exe`.
        let cfg = Config {
            data_dir: "/nonexistent_dir_for_test".into(),
            ..Default::default()
        };
        let p = cfg.python_path();
        if cfg!(windows) {
            assert_eq!(p, "python");
        } else {
            assert_eq!(p, "python3");
        }
    }

    #[test]
    fn faster_whisper_model_arg_falls_back_to_name() {
        let cfg = Config {
            data_dir: "/nonexistent".into(),
            whisper_model: "small.en".into(),
            ..Default::default()
        };
        // Local model doesn't exist, should return bare name
        assert_eq!(cfg.faster_whisper_model_arg(), "small.en");
    }

    #[test]
    fn qe_model_falls_back_to_main_model() {
        let mut cfg = Config {
            model: "gpt-4".into(),
            ..Default::default()
        };
        assert_eq!(cfg.qe_model_or_default(), "gpt-4");
        cfg.qe_model = "gpt-3.5".into();
        assert_eq!(cfg.qe_model_or_default(), "gpt-3.5");
    }

    #[test]
    fn endpoint_overrides_fall_back() {
        let mut cfg = Config {
            api_key: "shared".into(),
            base_url: "https://shared".into(),
            ..Default::default()
        };
        // Empty overrides → fall back to shared.
        assert_eq!(cfg.asr_api_key(), "shared");
        assert_eq!(cfg.asr_base_url(), "https://shared");
        assert_eq!(cfg.qe_api_key(), "shared");
        assert_eq!(cfg.qe_base_url(), "https://shared");
        // Override only ASR.
        cfg.asr_api_key = "asr-only".into();
        cfg.asr_base_url = "https://asr".into();
        assert_eq!(cfg.asr_api_key(), "asr-only");
        assert_eq!(cfg.asr_base_url(), "https://asr");
        // QE still falls back.
        assert_eq!(cfg.qe_api_key(), "shared");
    }

    #[test]
    fn set_validates_enum_values() {
        let mut cfg = Config::default();
        assert!(cfg.set("asr", "invalid").is_err());
        assert!(cfg.set("asr", "bijian").is_ok());
        assert!(cfg.set("whisper_device", "tpu").is_err());
        assert!(cfg.set("whisper_device", "cuda").is_ok());
        assert!(cfg.set("cuda_gpu", "2").is_ok());
        assert!(cfg.set("cuda_gpu", "").is_ok());
        assert!(cfg.set("cuda_gpu", "cuda:0").is_err());
    }

    #[test]
    fn set_rejects_zero_concurrency_values() {
        // 0 thread_num would deadlock (Semaphore::new(0) never permits)
        // and 0 batch_size would panic Vec::chunks(0).
        let mut cfg = Config::default();
        assert!(
            cfg.set("thread_num", "0").is_err(),
            "thread_num=0 must be rejected"
        );
        assert!(
            cfg.set("batch_size", "0").is_err(),
            "batch_size=0 must be rejected"
        );
        assert!(cfg.set("thread_num", "1").is_ok());
        assert!(cfg.set("batch_size", "1").is_ok());
    }

    #[test]
    fn safe_thread_num_clamps_to_one() {
        // Defense-in-depth: even if a hand-edited TOML or older binary left
        // 0 in the file, the runtime use-site must still operate.
        let mut cfg = Config {
            thread_num: 0,
            ..Default::default()
        };
        assert_eq!(cfg.safe_thread_num(), 1);
        cfg.thread_num = 5;
        assert_eq!(cfg.safe_thread_num(), 5);
    }

    #[test]
    fn safe_batch_size_clamps_to_one() {
        let mut cfg = Config {
            batch_size: 0,
            ..Default::default()
        };
        assert_eq!(cfg.safe_batch_size(), 1);
        cfg.batch_size = 7;
        assert_eq!(cfg.safe_batch_size(), 7);
    }

    #[test]
    fn redact_short_secret_does_not_leak() {
        // Regression: 3-char keys used to leak in full because of a flawed
        // truncation. Verify we never disclose a key shorter than 5 chars.
        assert_eq!(redact_secret(""), "(not set)");
        assert_eq!(redact_secret("abc"), "***");
        assert_eq!(redact_secret("abcd"), "***");
        assert_eq!(redact_secret("abcde"), "abcd...");
    }
}
