//! CLI argument structure.
//!
//! Kept separate from `main.rs` so the dispatch and the argument shape can
//! evolve independently, and so individual command handlers can take typed
//! arg structs in tests rather than parsing argv.

use clap::{ArgAction, Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "subforge", version, about = "视频字幕处理 CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
    /// 配置文件路径
    #[arg(long, global = true, help_heading = "Global Options")]
    pub config: Option<PathBuf>,
    /// 静默：只输出错误
    #[arg(
        short = 'q',
        long,
        global = true,
        action = ArgAction::SetTrue,
        help_heading = "Global Options"
    )]
    pub quiet: bool,
    /// 详细：打印每批/每条诊断信息
    #[arg(
        short = 'v',
        long,
        global = true,
        action = ArgAction::SetTrue,
        help_heading = "Global Options"
    )]
    pub verbose: bool,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // Synthesize legitimately has many fields; a Box would obscure the dispatch.
pub enum Commands {
    /// 语音识别（音频/视频 → SRT）
    Transcribe {
        /// 输入音频或视频文件
        input: String,
        /// 输出 SRT 路径（默认与输入同目录、同名 .srt）
        #[arg(short, long)]
        output: Option<String>,
        /// ASR 后端覆盖：bijian / faster-whisper / whisper-api / whisper-cpp
        #[arg(long)]
        asr: Option<String>,
        /// 输出格式：srt（默认）；预留扩展点
        #[arg(long)]
        format: Option<String>,
    },
    /// 字幕翻译处理（SRT → SRT）
    Subtitle {
        /// 输入 SRT 文件
        input: String,
        /// 输出 SRT 路径（默认 <stem>_translated.srt）
        #[arg(short, long)]
        output: Option<String>,
        /// 翻译器覆盖：llm / google / bing
        #[arg(long)]
        translator: Option<String>,
        /// 目标语言代码（如 zh-Hans / en / ja）
        #[arg(long)]
        target_language: Option<String>,
        /// 布局：target-above / source-above / target-only / source-only
        #[arg(long)]
        layout: Option<String>,
        /// 不翻译，只重排版（保持原文）
        #[arg(long, action = ArgAction::SetTrue)]
        no_translate: bool,
        /// 翻译并发线程数
        #[arg(long)]
        thread_num: Option<u16>,
        /// LLM 翻译每批字幕条数
        #[arg(long)]
        batch_size: Option<u16>,
    },
    /// 字幕烧制（视频 + SRT → 带字幕视频）
    Synthesize {
        /// 输入视频文件
        input: String,
        /// 字幕文件（SRT）
        #[arg(long)]
        subtitle: String,
        /// 输出视频路径（默认 <stem>_captioned.<ext>）
        #[arg(short, long)]
        output: Option<String>,

        // ---- 模式 ----
        /// 烧制模式：hard=硬烧（默认）/ soft=软封装（无损 mux 字幕轨）/ both=两种各出一份。
        /// 留空时使用 config.toml 的 synth_mode（默认 hard）。
        #[arg(long)]
        mode: Option<String>,

        // ---- 字幕样式（仅 hard 模式生效） ----
        /// 字体名（如 "Source Han Sans" / "Microsoft YaHei"）
        #[arg(long)]
        font: Option<String>,
        /// 字号（像素）
        #[arg(long)]
        font_size: Option<u32>,
        /// 字体颜色 RRGGBB（如 FFFFFF）
        #[arg(long)]
        font_color: Option<String>,
        /// 描边颜色 RRGGBB
        #[arg(long)]
        outline_color: Option<String>,
        /// 描边宽度（像素）
        #[arg(long)]
        outline_width: Option<u32>,
        /// 字幕位置：bottom（默认）/ center / top
        #[arg(long)]
        position: Option<String>,
        /// 距画面边缘的垂直边距（像素）
        #[arg(long)]
        margin_v: Option<u32>,
        /// 原样透传 force_style（覆盖前面所有 --font* 选项）
        #[arg(long)]
        style: Option<String>,

        // ---- 编码（仅 hard 模式生效） ----
        /// 视频编码器：x264 / x265 / nvenc / nvenc-hevc / qsv / videotoolbox
        #[arg(long)]
        encoder: Option<String>,
        /// 质量参数（CRF / cq / qp，越小越清晰；x264 默认 23，x265 默认 28）
        #[arg(long)]
        crf: Option<u8>,
        /// 编码速度档位：veryfast / fast / medium / slow / veryslow
        #[arg(long)]
        preset: Option<String>,
        /// 最大码率（如 "8M"、"5000k"）；带 bufsize 自动设为 2 倍
        #[arg(long)]
        max_bitrate: Option<String>,
        /// 字幕占据的画面宽度百分比 (1-100)，留空使用 config 的 synth_width_ratio。推荐 90。
        #[arg(long)]
        width_ratio: Option<u8>,

        // ---- 范围裁剪 ----
        /// 起始时间（如 "60" 表示 60 秒，或 "00:01:00"）
        #[arg(long)]
        ss: Option<String>,
        /// 持续时长（如 "30" 表示 30 秒，或 "00:00:30"）
        #[arg(long)]
        duration: Option<String>,
    },
    /// 转录 + 翻译（不烧制字幕）
    Translate {
        /// 输入音频或视频文件
        input: String,
        /// 输出翻译 SRT 路径
        #[arg(short, long)]
        output: Option<String>,
        /// ASR 后端覆盖
        #[arg(long, help_heading = "Pipeline Options")]
        asr: Option<String>,
        /// 翻译器覆盖：llm / google / bing
        #[arg(long, help_heading = "Pipeline Options")]
        translator: Option<String>,
        /// 目标语言代码（如 zh-Hans / en）
        #[arg(long, help_heading = "Pipeline Options")]
        target_language: Option<String>,
        /// 跳过缓存（强制重新转录 / 翻译）
        #[arg(long, action = ArgAction::SetTrue, help_heading = "Pipeline Options")]
        no_cache: bool,
        /// 保留中间文件（默认会清理只剩最终输出）
        #[arg(long, action = ArgAction::SetTrue, help_heading = "Pipeline Options")]
        keep_intermediate: bool,
    },
    /// 全流程：transcribe → subtitle → synthesize
    Process {
        /// 输入视频文件
        input: String,
        /// 输出路径（文件名→单产物；目录→放进去；空→输入同目录）
        #[arg(short, long)]
        output: Option<String>,
        /// ASR 后端覆盖
        #[arg(long)]
        asr: Option<String>,
        /// 翻译器覆盖
        #[arg(long)]
        translator: Option<String>,
        /// 目标语言代码
        #[arg(long)]
        target_language: Option<String>,
        /// 跳过烧制步骤（仅产生翻译 SRT，等价 `subforge translate`）
        #[arg(long, action = ArgAction::SetTrue)]
        no_synthesize: bool,
        /// 跳过缓存
        #[arg(long, action = ArgAction::SetTrue)]
        no_cache: bool,
        /// 保留中间文件（转录 SRT、翻译 SRT）
        #[arg(long, action = ArgAction::SetTrue)]
        keep_intermediate: bool,

        // ---- 烧制选项（与 `subforge synthesize` 同名） ----
        /// 烧制模式：hard / soft / both（留空使用 config 的 synth_mode）
        #[arg(long)]
        synth_mode: Option<String>,
        #[arg(long)]
        font: Option<String>,
        #[arg(long)]
        font_size: Option<u32>,
        #[arg(long)]
        font_color: Option<String>,
        #[arg(long)]
        outline_color: Option<String>,
        #[arg(long)]
        outline_width: Option<u32>,
        #[arg(long)]
        position: Option<String>,
        #[arg(long)]
        margin_v: Option<u32>,
        #[arg(long)]
        style: Option<String>,
        #[arg(long)]
        encoder: Option<String>,
        #[arg(long)]
        crf: Option<u8>,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        max_bitrate: Option<String>,
        /// 字幕占据的画面宽度百分比 (1-100)，留空使用 config 的 synth_width_ratio。推荐 90。
        #[arg(long)]
        width_ratio: Option<u8>,
    },
    /// 批量处理多个视频
    #[command(after_help = "\
Examples:
  subforge batch translate videos/ --dry-run
  subforge batch translate videos/ -o out --recursive --jobs 2
  subforge batch process videos/ -o out --recursive --synth-mode soft

Use `subforge batch translate --help` or `subforge batch process --help` for mode-specific options.")]
    Batch {
        #[command(subcommand)]
        command: BatchCommand,
    },
    /// 配置管理
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// 模型管理
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// 探测 GPU，并选择 faster-whisper / NVENC 默认使用的 CUDA GPU
    Gpu {
        /// 非交互设置 GPU 编号（例如 0 / 1）
        #[arg(long)]
        set: Option<u32>,
    },
    /// 字幕质量评估 (SubER, IWSLT 2022)
    Eval {
        /// 待评估字幕 (SRT)
        hypothesis: String,
        /// 参考字幕 (SRT)
        #[arg(short, long)]
        reference: String,
        /// 语言（用于中日韩分词），可选: zh/ja/ko
        #[arg(short, long)]
        language: Option<String>,
    },
    /// 一键安装 Python 环境（创建 venv + 安装依赖 + PyTorch）
    Setup {
        /// PyTorch 计算后端：cpu（默认）/ cu124（CUDA 12.x）/ cu128-nightly（RTX 50 系列）
        #[arg(long, default_value = "cpu")]
        compute: String,
        /// 删除已有 venv 并重新创建
        #[arg(long, action = ArgAction::SetTrue)]
        force: bool,
    },
    /// 检查依赖（Python venv、ffmpeg、API 配置）
    Doctor,
    /// 缓存管理
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
}

#[derive(Subcommand)]
pub enum CacheCommand {
    /// 显示缓存状态
    Stats,
    /// 清空所有缓存
    Clean,
    /// 修剪缓存（按天数 / 大小上限）
    Prune {
        /// 删除超过 N 天未访问的条目
        #[arg(long)]
        days: Option<u64>,
        /// 缓存总大小不超过 N MB（保留最新条目）
        #[arg(long)]
        max_mb: Option<u64>,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // Process mirrors many existing process flags; boxing would make dispatch noisier.
pub enum BatchCommand {
    /// 批量转录 + 翻译（不烧制字幕）
    #[command(after_help = "\
Examples:
  subforge batch translate videos/ --dry-run
  subforge batch translate videos/ more.mp4 -o out
  subforge batch translate videos/ -o out --recursive --jobs 2 --report batch.json

Behavior:
  - Directory inputs scan one level by default; add --recursive for subdirectories.
  - Existing translated SRT outputs are skipped unless --overwrite is set.
  - With -o, output is treated as a directory root, never as a single file path.")]
    Translate {
        #[command(flatten)]
        common: BatchCommon,
        /// ASR 后端覆盖
        #[arg(long, help_heading = "Pipeline Options")]
        asr: Option<String>,
        /// 翻译器覆盖：llm / google / bing
        #[arg(long, help_heading = "Pipeline Options")]
        translator: Option<String>,
        /// 目标语言代码（如 zh-Hans / en）
        #[arg(long, help_heading = "Pipeline Options")]
        target_language: Option<String>,
        /// 跳过缓存（强制重新转录 / 翻译）
        #[arg(long, action = ArgAction::SetTrue, help_heading = "Pipeline Options")]
        no_cache: bool,
        /// 保留中间文件（默认会清理只剩最终输出）
        #[arg(long, action = ArgAction::SetTrue, help_heading = "Pipeline Options")]
        keep_intermediate: bool,
    },
    /// 批量全流程：transcribe → subtitle → synthesize
    #[command(after_help = "\
Examples:
  subforge batch process videos/ --dry-run
  subforge batch process videos/ -o out --recursive --synth-mode soft
  subforge batch process ep1.mp4 ep2.mp4 -o out --synth-mode both

Behavior:
  - Directory inputs scan one level by default; add --recursive for subdirectories.
  - Existing final video outputs are skipped unless --overwrite is set.
  - With --synth-mode both, both hard-burn and soft-subtitle outputs must exist to skip.")]
    Process {
        #[command(flatten)]
        common: BatchCommon,
        /// ASR 后端覆盖
        #[arg(long, help_heading = "Pipeline Options")]
        asr: Option<String>,
        /// 翻译器覆盖
        #[arg(long, help_heading = "Pipeline Options")]
        translator: Option<String>,
        /// 目标语言代码
        #[arg(long, help_heading = "Pipeline Options")]
        target_language: Option<String>,
        /// 跳过烧制步骤（仅产生翻译 SRT，等价 batch translate）
        #[arg(long, action = ArgAction::SetTrue, help_heading = "Pipeline Options")]
        no_synthesize: bool,
        /// 跳过缓存
        #[arg(long, action = ArgAction::SetTrue, help_heading = "Pipeline Options")]
        no_cache: bool,
        /// 保留中间文件（转录 SRT、翻译 SRT）
        #[arg(long, action = ArgAction::SetTrue, help_heading = "Pipeline Options")]
        keep_intermediate: bool,

        // ---- 烧制选项（与 `subforge process` 同名） ----
        /// 烧制模式：hard / soft / both（留空使用 config 的 synth_mode）
        #[arg(long, help_heading = "Synthesis Options")]
        synth_mode: Option<String>,
        /// 字体名（如 "Source Han Sans" / "Microsoft YaHei"）
        #[arg(long, help_heading = "Synthesis Options")]
        font: Option<String>,
        /// 字号（像素）
        #[arg(long, help_heading = "Synthesis Options")]
        font_size: Option<u32>,
        /// 字体颜色 RRGGBB（如 FFFFFF）
        #[arg(long, help_heading = "Synthesis Options")]
        font_color: Option<String>,
        /// 描边颜色 RRGGBB
        #[arg(long, help_heading = "Synthesis Options")]
        outline_color: Option<String>,
        /// 描边宽度（像素）
        #[arg(long, help_heading = "Synthesis Options")]
        outline_width: Option<u32>,
        /// 字幕位置：bottom / center / top / top-right 等
        #[arg(long, help_heading = "Synthesis Options")]
        position: Option<String>,
        /// 距画面边缘的垂直边距（像素）
        #[arg(long, help_heading = "Synthesis Options")]
        margin_v: Option<u32>,
        /// 原样透传 force_style（覆盖前面所有 --font* 选项）
        #[arg(long, help_heading = "Synthesis Options")]
        style: Option<String>,
        /// 视频编码器：x264 / x265 / nvenc / nvenc-hevc / qsv / videotoolbox
        #[arg(long, help_heading = "Synthesis Options")]
        encoder: Option<String>,
        /// 质量参数（CRF / cq / qp，越小越清晰）
        #[arg(long, help_heading = "Synthesis Options")]
        crf: Option<u8>,
        /// 编码速度档位：veryfast / fast / medium / slow / veryslow
        #[arg(long, help_heading = "Synthesis Options")]
        preset: Option<String>,
        /// 最大码率（如 "8M"、"5000k"）
        #[arg(long, help_heading = "Synthesis Options")]
        max_bitrate: Option<String>,
        /// 字幕占据的画面宽度百分比 (1-100)，留空使用 config 的 synth_width_ratio。推荐 90。
        #[arg(long, help_heading = "Synthesis Options")]
        width_ratio: Option<u8>,
    },
}

#[derive(Args, Debug, Clone)]
pub struct BatchCommon {
    /// 输入视频文件或目录，可传多个
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// 输出目录根（批量模式只按目录解释）
    #[arg(short, long, help_heading = "Batch Options")]
    pub output: Option<String>,
    /// 递归扫描输入目录
    #[arg(long, action = ArgAction::SetTrue, help_heading = "Batch Options")]
    pub recursive: bool,
    /// 同时处理的视频数
    #[arg(
        long,
        default_value_t = 1,
        value_parser = parse_positive_usize,
        help_heading = "Batch Options"
    )]
    pub jobs: usize,
    /// 已有最终产物时仍然重跑
    #[arg(long, action = ArgAction::SetTrue, help_heading = "Batch Options")]
    pub overwrite: bool,
    /// 只打印计划，不实际执行
    #[arg(long, action = ArgAction::SetTrue, help_heading = "Batch Options")]
    pub dry_run: bool,
    /// 写入 JSON 报告路径
    #[arg(long, help_heading = "Batch Options")]
    pub report: Option<String>,
}

fn parse_positive_usize(s: &str) -> Result<usize, String> {
    let value = s
        .parse::<usize>()
        .map_err(|e| format!("invalid positive integer: {e}"))?;
    if value == 0 {
        Err("value must be at least 1".into())
    } else {
        Ok(value)
    }
}

#[derive(Subcommand)]
pub enum ModelCommand {
    /// 列出可用模型
    List,
    /// 下载模型
    Download { name: Option<String> },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// 显示配置
    Show,
    /// 获取配置值
    Get { key: String },
    /// 设置配置值
    Set { key: String, value: String },
    /// 显示配置文件路径
    Path,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_batch_translate_dry_run() {
        let cli =
            Cli::try_parse_from(["subforge", "batch", "translate", "--dry-run", "a.mp4"]).unwrap();
        match cli.command {
            Commands::Batch {
                command:
                    BatchCommand::Translate {
                        common, no_cache, ..
                    },
            } => {
                assert!(common.dry_run);
                assert_eq!(common.jobs, 1);
                assert!(!no_cache);
                assert_eq!(common.inputs, vec!["a.mp4"]);
            }
            _ => panic!("expected batch translate"),
        }
    }

    #[test]
    fn parses_batch_process_recursive_output_and_jobs() {
        let cli = Cli::try_parse_from([
            "subforge",
            "batch",
            "process",
            "--recursive",
            "-o",
            "out",
            "--jobs",
            "2",
            "videos",
        ])
        .unwrap();
        match cli.command {
            Commands::Batch {
                command: BatchCommand::Process { common, .. },
            } => {
                assert!(common.recursive);
                assert_eq!(common.output.as_deref(), Some("out"));
                assert_eq!(common.jobs, 2);
                assert_eq!(common.inputs, vec!["videos"]);
            }
            _ => panic!("expected batch process"),
        }
    }

    #[test]
    fn rejects_zero_batch_jobs() {
        let result =
            Cli::try_parse_from(["subforge", "batch", "translate", "--jobs", "0", "a.mp4"]);
        let err = match result {
            Ok(_) => panic!("expected --jobs 0 to be rejected"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("at least 1"));
    }
}
