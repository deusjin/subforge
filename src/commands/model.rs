use std::path::Path;

use crate::config::Config;

/// Default upstream HuggingFace org for faster-whisper model weights.
/// Override with `cfg.hf_model_repo_prefix` if you need a mirror or fork.
const DEFAULT_HF_PREFIX: &str = "Systran";

/// (model_name, size_label, repo_suffix). The `repo_suffix` joins onto the
/// configured `hf_model_repo_prefix` so users can swap orgs without a code change.
const MODELS: &[(&str, &str, &str)] = &[
    ("tiny", "39M", "faster-whisper-tiny"),
    ("tiny.en", "39M", "faster-whisper-tiny.en"),
    ("base", "74M", "faster-whisper-base"),
    ("base.en", "74M", "faster-whisper-base.en"),
    ("small", "244M", "faster-whisper-small"),
    ("small.en", "244M", "faster-whisper-small.en"),
    ("medium", "769M", "faster-whisper-medium"),
    ("medium.en", "769M", "faster-whisper-medium.en"),
    ("large", "1550M", "faster-whisper-large-v3"),
    ("turbo", "809M", "faster-whisper-large-v3-turbo"),
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
            println!("  {:<10} {:<6} 状态", "名称", "大小");
            println!("  {:-<10} {:-<6} ----", "", "");
            for (name, size, _) in MODELS {
                let installed = models_dir.join(name).join("model.bin").exists();
                let status = if installed {
                    "✓ 已安装"
                } else {
                    "未安装"
                };
                println!("  {name:<10} {size:<6} {status}");
            }
            println!("\n使用 `subforge model download <名称>` 下载模型");
            Ok(())
        }
        super::ModelCommand::Download { name } => {
            let name = match name {
                Some(n) => n,
                None => prompt_model_choice(MODELS, &models_dir)?,
            };
            let (_, size, repo_suffix) = MODELS
                .iter()
                .find(|(n, _, _)| *n == name)
                .ok_or_else(|| format!("未知模型: {name}"))?;
            let repo = repo_id(cfg, repo_suffix);

            let dest = models_dir.join(&name);
            if dest.join("model.bin").exists() {
                println!("模型 {name} 已存在于 {}", dest.display());
                return Ok(());
            }

            println!("下载模型 {name} ({size}) 从 huggingface.co/{repo} ...");
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

            let python = cfg.python_path();
            // Use a list of args so the repo id is passed as data, not interpolated
            // into a shell template — defends against unusual prefix values.
            let script = r#"
import sys
from huggingface_hub import snapshot_download
snapshot_download(repo_id=sys.argv[1], local_dir=sys.argv[2])
"#;
            // Capture stderr so a failed download's actual cause (HF 401,
            // network error, disk full) reaches the user instead of an
            // opaque "模型下载失败".
            let output = tokio::process::Command::new(&python)
                .args(["-c", script, &repo, &dest.to_string_lossy()])
                .stderr(std::process::Stdio::piped())
                .stdout(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|e| format!("Python 未找到: {e}"))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let tail: String = stderr
                    .lines()
                    .rev()
                    .take(15)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                let code = output
                    .status
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
    }
}

fn prompt_model_choice(models: &[(&str, &str, &str)], models_dir: &Path) -> Result<String, String> {
    println!("请选择要下载的模型:\n");
    for (i, (name, size, _)) in models.iter().enumerate() {
        let installed = models_dir.join(name).join("model.bin").exists();
        let mark = if installed { " ✓" } else { "" };
        println!("  {} - {} ({}){}", i + 1, name, size, mark);
    }
    println!();
    eprint!("输入编号 (1-{}): ", models.len());
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| e.to_string())?;
    let idx: usize = input
        .trim()
        .parse::<usize>()
        .map_err(|_| "无效编号")?
        .checked_sub(1)
        .ok_or("无效编号")?;
    if idx >= models.len() {
        return Err("无效编号".into());
    }
    Ok(models[idx].0.to_string())
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
}
