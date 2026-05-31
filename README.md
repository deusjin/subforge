<div align="center">
  <img src="assets/logo-compact.png" alt="SubForge" height="96"><br>
  <strong>Turn video subtitle production into a reproducible AI pipeline.</strong><br>
  <a href="https://github.com/deusjin/subforge/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/deusjin/subforge/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-blue.svg"></a>
  <a href="Cargo.toml"><img alt="Rust 1.88+" src="https://img.shields.io/badge/rust-1.88%2B-orange.svg"></a><br>
  <a href="README.md">English</a> |
  <a href="README.zh-CN.md">简体中文</a>
</div>

SubForge is a Rust CLI for transcribing, segmenting, translating, evaluating,
and muxing or burning subtitles into videos. It is built for people who process
videos repeatedly and do not want every project to become a pile of scripts,
temporary files, model paths, ffmpeg flags, and manual rework.

```text
video / audio
  -> speech recognition
  -> subtitle segmentation
  -> translation
  -> quality estimation
  -> hard-burned video / soft subtitle track
```

<p align="center">
  <img src="assets/result.png" alt="SubForge bilingual subtitle output preview" width="900">
</p>

## Why SubForge

Most subtitle workflows are not one tool. They are a chain:

- extract or transcribe audio
- split text into readable subtitle cues
- translate with enough context to keep terms stable
- check low-quality translations
- render subtitles into a video or mux them as a subtitle track
- keep intermediate outputs, caches, models, and project memory organized

SubForge makes that chain explicit, repeatable, and inspectable.

It is not a GUI editor. It is a CLI-first tool for local automation,
batch processing, video localization, course translation, and creator
workflows where reproducibility matters.

## Highlights

- Rust CLI with Linux, macOS, and Windows CI
- Local `faster-whisper` transcription with CPU or CUDA support
- SaT-based subtitle segmentation via an embedded Python sidecar
- Google, Bing, and OpenAI-compatible LLM translation backends
- Two LLM translation modes:
  - chained translation for best context continuity
  - wave-based concurrent translation for long videos
- MAPS-style terminology extraction and project-level translation memory
- GEMBA-MQM quality estimation with targeted low-score refinement
- Hard subtitle burning and soft subtitle muxing through ffmpeg
- GPU selection for faster-whisper and NVENC encoders
- Model download, cache management, environment diagnostics, and config tools
- Secret scanning in CI

## Status

SubForge is usable, but still early. The current release is `0.2.0`.

The core CLI, ffmpeg integration, cache handling, model management, and
configuration flow are already in place. The next layer of work is better
release automation, broader real-world end-to-end testing, and more polished
documentation for recommended translation settings.

## Installation

SubForge currently builds from source. Prebuilt binaries are not published yet.

### Requirements

| Tool | Purpose |
| --- | --- |
| Rust 1.88+ | Build the `subforge` binary |
| Python 3.9+ | Run faster-whisper, SaT, SubER, and related sidecars |
| ffmpeg | Extract audio, burn subtitles, and mux subtitle tracks |

Install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal # Linux / macOS
```

Windows PowerShell:

```powershell
$env:RUSTUP_DIST_SERVER="https://rsproxy.cn"
$env:RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"

winget install --id Rustlang.Rustup --exact --force

rustup set profile minimal
rustup toolchain install stable
rustup default stable
cargo --version

mkdir $env:USERPROFILE\.cargo -Force
@"
[source.crates-io]
replace-with = "rsproxy"

[source.rsproxy]
registry = "sparse+https://rsproxy.cn/index/"
"@ | Set-Content -Encoding UTF8 $env:USERPROFILE\.cargo\config.toml
```

Install ffmpeg:

```bash
sudo apt install ffmpeg            # Debian / Ubuntu
sudo dnf install ffmpeg            # Fedora
sudo pacman -S ffmpeg              # Arch
brew install ffmpeg                # macOS
winget install Gyan.FFmpeg         # Windows
```

Install SubForge:

```bash
git clone https://github.com/deusjin/subforge.git
cd subforge
cargo install --path .
subforge --version
```

Install Python-side dependencies:

```bash
subforge setup                         # CPU PyTorch
subforge setup --compute cu124         # CUDA 12.x
subforge setup --compute cu128-nightly # RTX 50 / Blackwell sm_120
subforge setup --force                 # rebuild the local venv
```

Create local config:

```bash
cp config.toml.example config.toml      # Linux / macOS
copy config.toml.example config.toml    # Windows cmd
```

Then verify the environment:

```bash
subforge doctor
```

`config.toml` is ignored by Git because it may contain API keys.

## Quick Start

Process a video end to end:

```bash
subforge process video.mp4
```

Generate translated subtitles without burning them into the video:

```bash
subforge translate video.mp4
```

Use soft subtitle muxing instead of re-encoding the video:

```bash
subforge process video.mp4 --synth-mode soft
```

Generate both a hard-burned video and a soft-subtitle output:

```bash
subforge process video.mp4 --synth-mode both
```

Run each stage manually:

```bash
subforge transcribe video.mp4 --asr faster-whisper
subforge subtitle video.srt --translator google
subforge synthesize video.mp4 --subtitle video_translated.srt
```

## CLI Overview

| Command | Purpose |
| --- | --- |
| `transcribe` | Audio/video to SRT |
| `subtitle` | SRT to translated SRT |
| `translate` | Transcribe and translate, without video synthesis |
| `synthesize` | Video + SRT to hard-burned or muxed output |
| `process` | Full pipeline: transcribe, translate, synthesize |
| `eval` | Subtitle quality evaluation with SubER and text metrics |
| `setup` | Create Python venv and install sidecar dependencies |
| `doctor` | Check ffmpeg, Python packages, CUDA, config, and sidecar sync |
| `model` | List and download faster-whisper models |
| `gpu` | Detect and select the default CUDA GPU |
| `cache` | Show, prune, or clean cache entries |
| `config` | Show, get, set, and locate configuration |

## Translation Backends

| Backend | Mode | Notes |
| --- | --- | --- |
| `google` | Web translation | Free, no key required, simple concurrent per-cue path |
| `bing` | Web translation | Free, requires `curl`, uses refreshable auth token handling |
| `llm` | OpenAI-compatible API | Best quality path, supports terminology, memory, QE, and refine |
| empty string | No translation | Useful for reformatting or synthesis only |

Example:

```bash
subforge subtitle input.srt --translator llm --target-language zh-Hans
```

For LLM translation, configure:

```toml
translator      = "llm"
api_key         = ""
base_url        = "https://api.openai.com/v1"
model           = "gpt-4o-mini"
target_language = "zh-Hans"
```

Environment variables take precedence:

```bash
export OPENAI_API_KEY="..."
export OPENAI_BASE_URL="https://api.openai.com/v1"
```

## LLM Quality Pipeline

The LLM backend uses a staged pipeline:

```text
source SRT
  |
  +-- terminology extraction
  |     -> glossary.jsonl
  |
  +-- batched translation
  |     -> chained mode or concurrent wave mode
  |
  +-- GEMBA-MQM quality estimation
  |
  +-- targeted refinement for low-score cues
  |
  +-- translation memory
        -> memory.jsonl
```

`chained_translation = true` gives the best continuity because every batch sees
the previous translations before it runs. It is effectively sequential for the
main LLM translation stage.

`chained_translation = false` warms up with three sequential batches and then
runs later batches in concurrent waves. It is faster on long videos, while
preserving cross-wave context.

```toml
chained_translation = true   # best continuity
thread_num          = 3
batch_size          = 7
```

For faster long-video runs:

```toml
chained_translation = false
thread_num          = 5
batch_size          = 10
```

`batch_size` controls how many subtitle cues go into one LLM request.
`thread_num` controls how many requests can run at the same time.

## ASR Backends

| Backend | Description | Configuration |
| --- | --- | --- |
| `faster-whisper` | Local Whisper transcription, recommended | `whisper_model`, `whisper_device` |
| `bijian` | Bilibili Bcut endpoint | `bijian_base_url` |
| `whisper-api` | OpenAI-compatible audio transcription | `whisper_api_model`, `asr_api_key`, `asr_base_url` |
| `whisper-cpp` | Local whisper.cpp binary | `whisper_cpp_model` |

First-time local model usage is interactive. SubForge lists available
faster-whisper models and downloads the selected one with Hugging Face progress.
For non-interactive environments, download in advance:

```bash
subforge model list
subforge model download turbo
```

## Subtitle Synthesis

Hard burn subtitles:

```bash
subforge synthesize video.mp4 --subtitle sub.srt --mode hard
```

Soft-mux subtitles without re-encoding:

```bash
subforge synthesize video.mp4 --subtitle sub.srt --mode soft
```

Create both outputs:

```bash
subforge synthesize video.mp4 --subtitle sub.srt --mode both
```

Style and encoder options:

```bash
subforge synthesize video.mp4 --subtitle sub.srt \
  --font "Source Han Sans" \
  --font-size 24 \
  --font-color FFFFFF \
  --outline-color 000000 \
  --outline-width 3 \
  --position bottom-right \
  --encoder x265 \
  --crf 22 \
  --preset slow
```

Soft subtitle container behavior:

| Output container | Subtitle codec |
| --- | --- |
| `.mp4`, `.m4v`, `.mov` | `mov_text` |
| `.mkv` | SRT |
| `.webm` | WebVTT, only when the source video is already WebM-compatible |

If in doubt, use `.mkv` for soft subtitles or use hard burn.

## GPU Selection

SubForge can pin faster-whisper and NVENC to a selected CUDA GPU:

```bash
subforge gpu
subforge gpu --set 1
subforge config get cuda_gpu
subforge config set cuda_gpu ""
```

At runtime, SubForge reports the physical GPU selected through
`CUDA_VISIBLE_DEVICES`.

## Configuration

Config lookup order:

1. `--config <path>`
2. `$SUBFORGE_CONFIG`
3. `./config.toml`
4. `$XDG_CONFIG_HOME/subforge/config.toml`
5. `$HOME/.config/subforge/config.toml`

Common options:

| Key | Default | Description |
| --- | --- | --- |
| `asr` | `faster-whisper` | Speech recognition backend |
| `whisper_model` | `small.en` | faster-whisper model |
| `segmenter` | `sat` | Subtitle segmentation algorithm |
| `translator` | `bing` | Translation backend |
| `target_language` | `zh-Hans` | Target language |
| `layout` | `target-above` | Subtitle layout |
| `thread_num` | `3` | Concurrent request count |
| `batch_size` | `7` | LLM subtitle cues per request |
| `quality_estimation` | `true` | Enable GEMBA-MQM scoring |
| `refine` | `true` | Retry low-score translations |
| `synth_mode` | empty | `hard`, `soft`, or `both`; empty means hard |
| `synth_encoder` | empty | `x264`, `x265`, `nvenc`, `nvenc-hevc`, `qsv`, `videotoolbox` |

Use:

```bash
subforge config show
subforge config get api_key
subforge config set whisper_model medium
subforge config path
```

`subforge config get api_key` only prints a redacted prefix.

## Translation Memory

By default, project memory is stored beside the source video:

```text
.subforge-tm/
  glossary.jsonl
  memory.jsonl
  .lock
```

For shared memory across multiple videos, set:

```bash
subforge config set tm_dir /shared/path/.subforge-tm
```

The memory writer uses an advisory lock so concurrent processes do not corrupt
the JSONL files.

## Cache and Model Data

Runtime data lives under `.subforge/` by default:

```text
.subforge/
  cache/
  models/faster-whisper/
  tools/faster-whisper-cli/venv/
```

Useful commands:

```bash
subforge cache stats
subforge cache prune --days 30 --max-mb 500
subforge cache clean
```

## Evaluation

Evaluate subtitle quality against a reference SRT:

```bash
subforge eval output.srt -r reference.srt
subforge eval output.srt -r reference.srt -l zh
```

The eval path uses SubER plus text metrics such as WER, BLEU, chrF, and TER.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -A clippy::field_reassign_with_default
cargo build --all-targets
cargo test --all-targets
cargo bench --bench client_reuse
```

The Python sidecar at `scripts/transcribe_segment.py` is embedded into the Rust
binary with `include_str!`. Rebuild the binary after editing it. `subforge doctor`
checks whether the embedded copy and the file on disk are in sync.

## Security

- `config.toml` is ignored by Git and may contain API keys.
- `.subforge/`, `.subforge-tm/`, `target/`, and test videos are ignored.
- CI runs gitleaks against common API keys, bearer tokens, and JWTs.
- Runtime retry errors redact secret-looking URL query parameters and bearer
  tokens before printing them.

Do not paste `config.toml`, `subforge config show`, or API gateway URLs with
tokens into public issues.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `cargo` not found | Install Rust with rustup, then restart the terminal |
| `ffmpeg not found` | Install ffmpeg with your system package manager |
| Python package missing | Run `subforge setup` |
| CUDA unavailable | Rebuild the venv with `subforge setup --compute cu124 --force` |
| RTX 50 / Blackwell unsupported | Use `subforge setup --compute cu128-nightly --force` |
| `subforge eval` cannot find SubER | Run `subforge setup` or install `subtitle-edit-rate` |
| Bing translation fails because curl is missing | Install `curl` |
| Edited Python sidecar has no effect | Rebuild with `cargo install --path .` |

For a full local check:

```bash
subforge doctor
```

## Research References

SubForge is engineering-oriented, but several stages are inspired by published
subtitle and translation quality work:

- Segment Any Text, EMNLP 2024 - neural segmentation
- Long-Form Speech Translation, Findings of EMNLP 2023 - context-aware subtitle work
- GEMBA-MQM, WMT 2023 - LLM-based quality estimation
- MAPS, WMT 2024 - terminology extraction and consistency
- SubER, IWSLT 2022 - subtitle evaluation

## Community

This project is promoted in the LINUX DO open-source community. Thanks to the
community for discussion, feedback, and suggestions.

- [LINUX DO](https://linux.do/)

## License

MIT
