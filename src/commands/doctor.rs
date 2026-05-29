use crate::config::Config;

pub async fn handle(cfg: &Config) -> Result<(), String> {
    println!("=== SUBFORGE 诊断 ===\n");
    let mut ok = true;

    // 1. ffmpeg — required for audio extraction + subtitle burning.
    let ffmpeg_ok = bin_runs("ffmpeg", &["-version"]).await;
    println!("[{}] ffmpeg", mark(ffmpeg_ok));
    if !ffmpeg_ok {
        println!(
            "    {}",
            crate::util::ffmpeg_install_hint().replace('\n', "\n    ")
        );
    }
    ok &= ffmpeg_ok;

    // 2. curl — used by the bing translator to fetch the auth token. Bing's
    //    edge endpoint TLS-fingerprints non-browser clients and rejects
    //    `reqwest` directly, so the bing path shells out to curl. If you
    //    don't use bing this is informational only.
    let curl_ok = bin_runs("curl", &["--version"]).await;
    let curl_required = cfg.translator == "bing";
    println!(
        "[{}] curl{}",
        if curl_ok {
            "✓"
        } else if curl_required {
            "✗"
        } else {
            "i"
        },
        if curl_ok {
            String::new()
        } else if curl_required {
            "  — translator=bing 需要 curl".to_string()
        } else {
            "  — 未安装（仅 bing 翻译需要）".to_string()
        }
    );
    if curl_required && !curl_ok {
        ok = false;
    }

    // 3. Python venv
    let python = cfg.python_path();
    let py_in_venv = cfg.venv_bin("python").exists();
    println!("[{}] python interpreter: {}", mark(py_in_venv), python);

    // 4. Python packages
    if py_in_venv {
        for pkg in &["faster_whisper", "wtpsplit", "torch", "transformers"] {
            let installed = tokio::process::Command::new(&python)
                .args(["-c", &format!("import {pkg}")])
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            println!("[{}] python pkg: {pkg}", mark(installed));
            if !installed {
                ok = false;
            }
        }
        // huggingface_hub is needed by `subforge model download`.
        let hf_ok = tokio::process::Command::new(&python)
            .args(["-c", "import huggingface_hub"])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        println!(
            "[{}] python pkg: huggingface_hub  — required by `subforge model download`",
            if hf_ok { "✓" } else { "i" }
        );

        let suber_ok = cfg.venv_bin("suber").exists();
        println!(
            "[{}] suber (subtitle-edit-rate)  — required by `subforge eval`",
            if suber_ok { "✓" } else { "i" }
        );
    } else {
        println!("    (跳过 python 包检查：未找到 venv)");
        println!("    运行 `subforge setup` 一键创建虚拟环境并安装依赖");
    }

    // 5. Model
    let model_dir = cfg
        .models_dir()
        .join("faster-whisper")
        .join(&cfg.whisper_model);
    let model_ok = model_dir.join("model.bin").exists();
    println!(
        "[{}] faster-whisper model: {} ({})",
        if model_ok { "✓" } else { "i" },
        cfg.whisper_model,
        if model_ok {
            "本地".to_string()
        } else {
            "首次使用时会自动从 HuggingFace 下载".to_string()
        }
    );

    // 6. LLM config — distinguishes shared vs per-task overrides.
    let shared_ok = !cfg.api_key.is_empty() && !cfg.base_url.is_empty();
    println!(
        "[{}] LLM 共享配置 (api_key + base_url){}",
        if shared_ok { "✓" } else { "i" },
        if shared_ok {
            String::new()
        } else if cfg.translator == "llm" {
            " — translator=llm 但未配置".to_string()
        } else {
            " — 未配置（使用 google/bing 翻译时不需要）".to_string()
        }
    );
    if !cfg.asr_api_key.is_empty() || !cfg.asr_base_url.is_empty() {
        let asr_complete = !cfg.asr_api_key().is_empty() && !cfg.asr_base_url().is_empty();
        println!(
            "[{}] LLM ASR 端点 (asr_api_key/asr_base_url 覆盖)",
            mark(asr_complete)
        );
    }
    if !cfg.qe_api_key.is_empty() || !cfg.qe_base_url.is_empty() {
        let qe_complete = !cfg.qe_api_key().is_empty() && !cfg.qe_base_url().is_empty();
        println!(
            "[{}] LLM QE 端点 (qe_api_key/qe_base_url 覆盖)",
            mark(qe_complete)
        );
    }

    // 7. GPU
    if py_in_venv {
        let gpu_check = tokio::process::Command::new(&python)
            .args(["-c", "import torch; print(torch.cuda.is_available())"])
            .output()
            .await;
        if let Ok(out) = gpu_check {
            let s = String::from_utf8_lossy(&out.stdout);
            let cuda = s.trim() == "True";
            println!("[{}] CUDA available", if cuda { "✓" } else { "i" });
        }
    }

    // 8. Embedded vs on-disk Python sidecar synchronization. The transcribe
    //    path uses an `include_str!`-embedded copy; the on-disk file at
    //    `scripts/transcribe_segment.py` is for human reading only. If the
    //    user has edited the on-disk version expecting it to take effect,
    //    they would silently keep getting the old behavior. Flag the drift.
    let on_disk = std::fs::read_to_string("scripts/transcribe_segment.py").ok();
    let embedded = crate::transcribe::embedded_transcribe_script();
    match on_disk {
        Some(disk) if disk == embedded => {
            println!(
                "[✓] python sidecar embedded copy is in sync with scripts/transcribe_segment.py"
            );
        }
        Some(_) => {
            println!(
                "[!] scripts/transcribe_segment.py 与已编译版本不同 — 修改后需 `cargo build` 重新编译，磁盘版本本身不会被加载"
            );
        }
        None => {
            // Not in a worktree (e.g. running from `cargo install`-built
            // binary in some other directory). That's normal; the embedded
            // copy is authoritative.
        }
    }

    println!();
    if ok {
        println!("一切正常 ✓");
        Ok(())
    } else {
        Err("发现问题，请按上方提示修复".into())
    }
}

async fn bin_runs(name: &str, args: &[&str]) -> bool {
    tokio::process::Command::new(name)
        .args(args)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn mark(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}
