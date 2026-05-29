use std::io::{self, IsTerminal, Write};
use std::path::Path;

use tokio::process::Command;

use crate::{config::Config, log_info};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuChoice {
    None,
    Auto(GpuInfo),
    Configured(GpuInfo),
    NeedsSelection(Vec<GpuInfo>),
}

pub fn parse_nvidia_smi_csv(output: &str) -> Result<Vec<GpuInfo>, String> {
    let mut gpus = Vec::new();
    for (line_no, line) in output.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if cols.len() < 2 {
            return Err(format!(
                "nvidia-smi 输出格式异常，第 {} 行: {line}",
                line_no + 1
            ));
        }
        let index = cols[0]
            .parse::<u32>()
            .map_err(|_| format!("无法解析 GPU 编号: {}", cols[0]))?;
        let memory_mb = cols.get(2).and_then(|s| s.parse::<u64>().ok());
        gpus.push(GpuInfo {
            index,
            name: cols[1].to_string(),
            memory_mb,
        });
    }
    Ok(gpus)
}

pub async fn detect_gpus() -> Result<Vec<GpuInfo>, String> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .await;

    let Ok(output) = output else {
        return Ok(Vec::new());
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    parse_nvidia_smi_csv(&String::from_utf8_lossy(&output.stdout))
}

pub fn choose_gpu(gpus: &[GpuInfo], configured: &str) -> Result<GpuChoice, String> {
    if !configured.trim().is_empty() {
        let index = configured
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("cuda_gpu 需要 GPU 编号，当前是: {configured}"))?;
        let gpu = gpus
            .iter()
            .find(|gpu| gpu.index == index)
            .cloned()
            .unwrap_or_else(|| GpuInfo {
                index,
                name: "未能通过 nvidia-smi 读取名称".into(),
                memory_mb: None,
            });
        return Ok(GpuChoice::Configured(gpu));
    }

    match gpus.len() {
        0 => Ok(GpuChoice::None),
        1 => Ok(GpuChoice::Auto(gpus[0].clone())),
        _ => Ok(GpuChoice::NeedsSelection(gpus.to_vec())),
    }
}

pub async fn handle_command(
    config_path: &Path,
    cfg: &Config,
    set: Option<u32>,
) -> Result<(), String> {
    let gpus = detect_gpus().await?;
    if gpus.is_empty() {
        println!("未检测到 NVIDIA GPU（nvidia-smi 不可用或无 CUDA 设备）。");
        return Ok(());
    }

    print_gpu_list(&gpus);
    let selected = match set {
        Some(index) => find_gpu(&gpus, index)?,
        None => prompt_gpu_choice(&gpus)?,
    };

    let mut cfg = cfg.clone();
    cfg.cuda_gpu = selected.index.to_string();
    cfg.save(config_path)?;
    println!(
        "✓ 默认 CUDA GPU 已设置为 [{}] {}",
        selected.index, selected.name
    );
    Ok(())
}

pub async fn prepare_for_faster_whisper(
    cfg: &mut Config,
    config_path: &Path,
) -> Result<(), String> {
    if cfg.asr != "faster-whisper" || cfg.whisper_device == "cpu" {
        return Ok(());
    }
    prepare_for_cuda_task(cfg, config_path, "Whisper GPU").await
}

pub async fn prepare_for_cuda_task(
    cfg: &mut Config,
    config_path: &Path,
    label: &str,
) -> Result<(), String> {
    let gpus = detect_gpus().await?;
    match choose_gpu(&gpus, &cfg.cuda_gpu)? {
        GpuChoice::None => {
            if cfg.whisper_device == "cuda" {
                log_info!("{label}: 未检测到 NVIDIA GPU；CUDA 任务可能会报不可用");
            } else {
                log_info!("{label}: 未检测到 NVIDIA GPU，任务将由底层工具决定是否回退 CPU");
            }
        }
        GpuChoice::Auto(gpu) | GpuChoice::Configured(gpu) => {
            log_info!("{label}: [{}] {}", gpu.index, gpu.name);
        }
        GpuChoice::NeedsSelection(gpus) => {
            print_gpu_list(&gpus);
            if !io::stdin().is_terminal() {
                return Err("检测到多张 GPU，但当前不是交互式终端。请先运行 `subforge gpu` 或 `subforge config set cuda_gpu <编号>`。".into());
            }
            let selected = prompt_gpu_choice(&gpus)?;
            cfg.cuda_gpu = selected.index.to_string();
            cfg.save(config_path)?;
            log_info!(
                "{label}: [{}] {}（已写入 config）",
                selected.index,
                selected.name
            );
        }
    }

    Ok(())
}

pub fn cuda_env(cfg: &Config) -> Vec<(&'static str, String)> {
    if cfg.cuda_gpu.trim().is_empty() {
        return Vec::new();
    }
    vec![("CUDA_VISIBLE_DEVICES", cfg.cuda_gpu.trim().to_string())]
}

pub fn print_gpu_list(gpus: &[GpuInfo]) {
    println!("检测到 GPU:\n");
    for gpu in gpus {
        match gpu.memory_mb {
            Some(memory) => println!("  [{}] {} ({} MB)", gpu.index, gpu.name, memory),
            None => println!("  [{}] {}", gpu.index, gpu.name),
        }
    }
    println!();
}

fn prompt_gpu_choice(gpus: &[GpuInfo]) -> Result<GpuInfo, String> {
    eprint!("请选择默认 Whisper GPU 编号: ");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| e.to_string())?;
    let index = input
        .trim()
        .parse::<u32>()
        .map_err(|_| "无效 GPU 编号".to_string())?;
    find_gpu(gpus, index)
}

fn find_gpu(gpus: &[GpuInfo], index: u32) -> Result<GpuInfo, String> {
    gpus.iter()
        .find(|gpu| gpu.index == index)
        .cloned()
        .ok_or_else(|| format!("未找到 GPU 编号 {index}"))
}
