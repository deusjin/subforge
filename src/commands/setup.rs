//! `subforge setup` — one-click Python environment bootstrap.
//!
//! Creates the project venv and installs every Python sidecar dependency
//! (faster-whisper, wtpsplit, transformers, huggingface_hub, subtitle-edit-rate)
//! plus PyTorch with the user-chosen compute backend. Works identically on
//! Linux / macOS / Windows by resolving the venv's python via
//! `Config::venv_bin`, which already handles `bin/` vs `Scripts\`.

use crate::config::Config;
use tokio::process::Command;

/// Pure pipeline deps (CPU-agnostic). torch/torchvision are installed
/// separately because they need a backend-specific `--index-url`.
const PIPELINE_PKGS: &[&str] = &[
    "faster-whisper",
    "wtpsplit",
    "subtitle-edit-rate",
    "transformers",
    "huggingface_hub",
];

pub async fn handle(compute: &str, force: bool, cfg: &Config) -> Result<(), String> {
    let venv = cfg.venv_dir();

    // 1. Create the venv (idempotent unless --force).
    if cfg.venv_bin("python").exists() && !force {
        println!("✓ venv 已存在: {}（加 --force 重建）", venv.display());
    } else {
        if force && venv.exists() {
            println!("移除旧 venv: {}", venv.display());
            std::fs::remove_dir_all(&venv).map_err(|e| format!("无法移除旧 venv: {e}"))?;
        }
        if let Some(parent) = venv.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("无法创建工具目录: {e}"))?;
        }
        let sys_python = if cfg!(windows) { "python" } else { "python3" };
        println!("[1/3] 创建虚拟环境: {}", venv.display());
        run(sys_python, &["-m", "venv", &venv.to_string_lossy()])
            .await
            .map_err(|e| {
                format!(
                    "{e}\n\n未找到系统 Python。请先安装 Python 3.9+：\n  {}",
                    python_install_hint()
                )
            })?;
    }

    let venv_python = cfg.venv_bin("python");
    let py = venv_python.to_string_lossy().into_owned();

    // 2. Upgrade pip, then install the pipeline packages.
    println!("[2/3] 安装管线依赖 ({}) ...", PIPELINE_PKGS.join(", "));
    run(&py, &["-m", "pip", "install", "--upgrade", "pip"]).await?;
    let mut pip_args = vec!["-m", "pip", "install"];
    pip_args.extend_from_slice(PIPELINE_PKGS);
    run(&py, &pip_args).await?;

    // 3. Install torch/torchvision for the chosen backend.
    println!("[3/3] 安装 PyTorch (compute={compute}) ...");
    let torch_args = torch_install_args(compute)?;
    let torch_argv: Vec<&str> = torch_args.iter().map(String::as_str).collect();
    run(&py, &torch_argv).await?;

    println!("\n✓ 安装完成。运行 `subforge doctor` 验证环境。");
    Ok(())
}

/// Build the `pip install` argv for torch given a compute backend label.
fn torch_install_args(compute: &str) -> Result<Vec<String>, String> {
    let mut a: Vec<String> = ["-m", "pip", "install"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match compute {
        "cpu" => {
            a.extend(["torch", "torchvision"].iter().map(|s| s.to_string()));
            a.extend(
                ["--index-url", "https://download.pytorch.org/whl/cpu"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
        "cu124" => {
            a.extend(["torch", "torchvision"].iter().map(|s| s.to_string()));
            a.extend(
                ["--index-url", "https://download.pytorch.org/whl/cu124"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
        // RTX 50 series (Blackwell sm_120) needs the nightly cu128 wheels.
        "cu128-nightly" => {
            a.push("--pre".into());
            a.extend(["torch", "torchvision"].iter().map(|s| s.to_string()));
            a.extend(
                [
                    "--index-url",
                    "https://download.pytorch.org/whl/nightly/cu128",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        }
        other => {
            return Err(format!(
                "未知 compute 后端: {other}。可选: cpu / cu124 / cu128-nightly"
            ));
        }
    }
    Ok(a)
}

fn python_install_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "winget install Python.Python.3.12  （或从 https://www.python.org/downloads/ 下载，安装时勾选 Add to PATH）"
    } else if cfg!(target_os = "macos") {
        "brew install python"
    } else {
        "sudo apt install python3 python3-venv   # Debian/Ubuntu"
    }
}

/// Spawn a command with inherited stdio so the user sees live pip progress,
/// and turn a non-zero exit or spawn failure into a readable error.
async fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let mut command = Command::new(program);
    command.args(args).kill_on_drop(true);
    if cfg!(windows) {
        command
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
    }
    let status = command
        .status()
        .await
        .map_err(|e| format!("无法执行 `{program}`: {e}"))?;
    if !status.success() {
        return Err(format!(
            "命令失败 (exit {}): {program} {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            args.join(" ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torch_cpu_uses_cpu_index() {
        let a = torch_install_args("cpu").unwrap();
        assert!(a.contains(&"https://download.pytorch.org/whl/cpu".to_string()));
        assert!(!a.contains(&"--pre".to_string()));
    }

    #[test]
    fn torch_cu128_is_nightly_prerelease() {
        let a = torch_install_args("cu128-nightly").unwrap();
        assert!(a.contains(&"--pre".to_string()));
        assert!(a.iter().any(|s| s.contains("nightly/cu128")));
    }

    #[test]
    fn torch_unknown_backend_errors() {
        assert!(torch_install_args("cu999").is_err());
    }
}
