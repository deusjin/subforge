use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncReadExt;

use crate::config::Config;

/// Default upstream HuggingFace org for faster-whisper model weights.
/// Override with `cfg.hf_model_repo_prefix` if you need a mirror or fork.
const DEFAULT_HF_PREFIX: &str = "Systran";

#[derive(Debug, Clone, Copy)]
pub struct WhisperModel {
    pub name: &'static str,
    pub size: &'static str,
    pub repo_suffix: &'static str,
    pub description: &'static str,
}

/// The `repo_suffix` joins onto the configured `hf_model_repo_prefix` so users
/// can swap orgs without a code change.
pub const MODELS: &[WhisperModel] = &[
    WhisperModel {
        name: "tiny",
        size: "39M",
        repo_suffix: "faster-whisper-tiny",
        description: "最快，质量最低，适合快速预览",
    },
    WhisperModel {
        name: "tiny.en",
        size: "39M",
        repo_suffix: "faster-whisper-tiny.en",
        description: "英文专用 tiny，最快，质量最低",
    },
    WhisperModel {
        name: "base",
        size: "74M",
        repo_suffix: "faster-whisper-base",
        description: "默认轻量模型，速度快，质量一般",
    },
    WhisperModel {
        name: "base.en",
        size: "74M",
        repo_suffix: "faster-whisper-base.en",
        description: "英文专用 base，轻量快速",
    },
    WhisperModel {
        name: "small",
        size: "244M",
        repo_suffix: "faster-whisper-small",
        description: "质量明显更好，仍然较快",
    },
    WhisperModel {
        name: "small.en",
        size: "244M",
        repo_suffix: "faster-whisper-small.en",
        description: "英文专用 small，质量和速度平衡",
    },
    WhisperModel {
        name: "medium",
        size: "769M",
        repo_suffix: "faster-whisper-medium",
        description: "质量较高，GPU 推荐",
    },
    WhisperModel {
        name: "medium.en",
        size: "769M",
        repo_suffix: "faster-whisper-medium.en",
        description: "英文专用 medium，质量较高",
    },
    WhisperModel {
        name: "large",
        size: "1550M",
        repo_suffix: "faster-whisper-large-v3",
        description: "质量最好，显存和时间占用最大",
    },
    WhisperModel {
        name: "turbo",
        size: "809M",
        repo_suffix: "faster-whisper-large-v3-turbo",
        description: "速度和质量折中，适合 GPU",
    },
];

fn repo_id(cfg: &Config, suffix: &str) -> String {
    let prefix = if cfg.hf_model_repo_prefix.is_empty() {
        DEFAULT_HF_PREFIX
    } else {
        cfg.hf_model_repo_prefix.as_str()
    };
    format!("{}/{}", prefix.trim_end_matches('/'), suffix)
}

pub async fn handle(cmd: super::ModelCommand, cfg: &Config) -> Result<(), String> {
    let models_dir = cfg.models_dir().join("faster-whisper");

    match cmd {
        super::ModelCommand::List => {
            println!(
                "可用 Whisper 模型 (HF prefix: {}):\n",
                if cfg.hf_model_repo_prefix.is_empty() {
                    DEFAULT_HF_PREFIX
                } else {
                    cfg.hf_model_repo_prefix.as_str()
                }
            );
            println!("  {:<9} {:>6}  状态      说明", "名称", "大小");
            println!("  {:-<9} {:->6}  --------  ----", "", "");
            for model in MODELS {
                println!(
                    "{}",
                    format_list_row(model, is_model_installed(&models_dir, model.name))
                );
            }
            println!("\n使用 `subforge model download <名称>` 下载模型");
            Ok(())
        }
        super::ModelCommand::Download { name } => {
            let name = match name {
                Some(n) => n,
                None => prompt_model_choice(MODELS, &models_dir)?,
            };
            download_model(cfg, &name).await
        }
    }
}

pub async fn ensure_faster_whisper_model(
    cfg: &mut Config,
    config_path: &Path,
) -> Result<(), String> {
    let models_dir = cfg.models_dir().join("faster-whisper");
    if is_model_installed(&models_dir, &cfg.whisper_model) {
        return Ok(());
    }

    println!(
        "Whisper 模型 `{}` 尚未安装。请选择要下载的模型:\n",
        cfg.whisper_model
    );
    let choice = prompt_model_choice(MODELS, &models_dir)?;
    if choice != cfg.whisper_model {
        cfg.whisper_model = choice.clone();
        cfg.save(config_path)?;
        println!(
            "✓ whisper_model = {choice} 已写入 {}",
            config_path.display()
        );
    }
    download_model(cfg, &choice).await
}

pub fn model_by_name(name: &str) -> Option<&'static WhisperModel> {
    MODELS.iter().find(|m| m.name == name)
}

pub fn model_dir(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join(name)
}

pub fn is_model_installed(models_dir: &Path, name: &str) -> bool {
    model_dir(models_dir, name).join("model.bin").exists()
}

fn prompt_model_choice(models: &[WhisperModel], models_dir: &Path) -> Result<String, String> {
    println!("请选择要下载的模型:\n");
    for (i, model) in models.iter().enumerate() {
        println!(
            "{}",
            format_choice_row(i + 1, model, is_model_installed(models_dir, model.name))
        );
    }
    println!();
    if !std::io::stdin().is_terminal() {
        return Err(
            "当前不是交互式终端，无法选择模型。请先运行: subforge model download <模型名>".into(),
        );
    }
    eprint!("输入编号 (1-{}): ", models.len());
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| e.to_string())?;
    parse_model_choice(&input, models)
}

fn parse_model_choice(input: &str, models: &[WhisperModel]) -> Result<String, String> {
    let trimmed = input.trim();
    if let Some(model) = model_by_name(trimmed) {
        return Ok(model.name.to_string());
    }
    let idx: usize = input
        .trim()
        .parse::<usize>()
        .map_err(|_| "无效编号")?
        .checked_sub(1)
        .ok_or("无效编号")?;
    if idx >= models.len() {
        return Err("无效编号".into());
    }
    Ok(models[idx].name.to_string())
}

pub async fn download_model(cfg: &Config, name: &str) -> Result<(), String> {
    let model = model_by_name(name).ok_or_else(|| format!("未知模型: {name}"))?;
    let models_dir = cfg.models_dir().join("faster-whisper");
    let dest = model_dir(&models_dir, name);
    if is_model_installed(&models_dir, name) {
        println!("模型 {name} 已下载，跳过下载: {}", dest.display());
        return Ok(());
    }

    let repo = repo_id(cfg, model.repo_suffix);
    println!(
        "下载模型 {} ({}) 从 huggingface.co/{} ...",
        model.name, model.size, repo
    );
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

    let python = cfg.python_path();
    let script = r#"
import sys
from huggingface_hub import snapshot_download
snapshot_download(repo_id=sys.argv[1], local_dir=sys.argv[2])
"#;
    let mut command = tokio::process::Command::new(&python);
    command
        .args(["-c", script, &repo, &dest.to_string_lossy()])
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if cfg!(windows) {
        command
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
    }
    let mut child = command.spawn().map_err(|e| format!("Python 未找到: {e}"))?;

    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "python stderr pipe missing".to_string())?;
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stderr.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            eprint!("{}", String::from_utf8_lossy(&buf[..n]));
            bytes.extend_from_slice(&buf[..n]);
        }
        Ok::<Vec<u8>, String>(bytes)
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("模型下载进程失败: {e}"))?;
    let stderr_bytes = stderr_task.await.map_err(|e| e.to_string())??;

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let tail: String = stderr
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into());
        return Err(format!(
            "模型下载失败 (exit {code})\n--- python stderr (tail) ---\n{tail}\n--- end ---"
        ));
    }
    println!("✓ 模型 {name} 已下载到 {}", dest.display());
    Ok(())
}

fn status_label(installed: bool) -> &'static str {
    if installed {
        "✓ 已下载"
    } else {
        "  未下载"
    }
}

fn format_list_row(model: &WhisperModel, installed: bool) -> String {
    format!(
        "  {:<9} {:>6}  {}  {}",
        model.name,
        model.size,
        status_label(installed),
        model.description
    )
}

fn format_choice_row(index: usize, model: &WhisperModel, installed: bool) -> String {
    format!(
        "  {:>2}. {:<9} {:>6}  {}  {}",
        index,
        model.name,
        model.size,
        status_label(installed),
        model.description
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_uses_default_when_empty() {
        let cfg = Config::default();
        assert_eq!(
            repo_id(&cfg, "faster-whisper-base"),
            "Systran/faster-whisper-base"
        );
    }

    #[test]
    fn repo_id_uses_override() {
        let cfg = Config {
            hf_model_repo_prefix: "BAAI".into(),
            ..Default::default()
        };
        assert_eq!(
            repo_id(&cfg, "faster-whisper-base"),
            "BAAI/faster-whisper-base"
        );
    }

    #[test]
    fn repo_id_strips_trailing_slash() {
        let cfg = Config {
            hf_model_repo_prefix: "myorg/".into(),
            ..Default::default()
        };
        assert_eq!(repo_id(&cfg, "x"), "myorg/x");
    }

    #[test]
    fn catalog_has_user_facing_descriptions() {
        let base = model_by_name("base").unwrap();
        assert!(base.description.contains("默认"), "{}", base.description);
        let turbo = model_by_name("turbo").unwrap();
        assert!(turbo.description.contains("GPU"), "{}", turbo.description);
    }

    #[test]
    fn installed_check_requires_model_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path();
        std::fs::create_dir_all(models_dir.join("base")).unwrap();
        assert!(!is_model_installed(models_dir, "base"));

        std::fs::write(models_dir.join("base").join("model.bin"), b"x").unwrap();
        assert!(is_model_installed(models_dir, "base"));
    }

    #[test]
    fn parse_model_choice_accepts_number_and_name() {
        assert_eq!(parse_model_choice("3", MODELS).unwrap(), "base");
        assert_eq!(parse_model_choice("turbo", MODELS).unwrap(), "turbo");
    }

    #[test]
    fn parse_model_choice_rejects_invalid_input() {
        assert!(parse_model_choice("999", MODELS).is_err());
        assert!(parse_model_choice("unknown", MODELS).is_err());
    }

    #[test]
    fn choice_rows_have_aligned_columns() {
        let row = format_choice_row(
            10,
            &WhisperModel {
                name: "turbo",
                size: "809M",
                repo_suffix: "x",
                description: "速度和质量折中，适合 GPU",
            },
            true,
        );
        assert_eq!(
            row,
            "  10. turbo       809M  ✓ 已下载  速度和质量折中，适合 GPU"
        );
    }
}
