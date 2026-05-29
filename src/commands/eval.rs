use std::path::PathBuf;

use crate::config::Config;

pub async fn handle(
    hypothesis: &str,
    reference: &str,
    language: Option<&str>,
    cfg: &Config,
) -> Result<(), String> {
    let hyp = PathBuf::from(hypothesis);
    let r = PathBuf::from(reference);
    if !hyp.exists() {
        return Err(format!("hypothesis not found: {hypothesis}"));
    }
    if !r.exists() {
        return Err(format!("reference not found: {reference}"));
    }

    let suber = cfg.venv_bin("suber");
    if !suber.exists() {
        return Err(format!(
            "suber not found at {}.\nInstall with:\n  {} -m pip install subtitle-edit-rate",
            suber.display(),
            cfg.python_path()
        ));
    }

    let mut cmd = tokio::process::Command::new(&suber);
    cmd.arg("-H").arg(&hyp);
    cmd.arg("-R").arg(&r);
    cmd.args(["-m", "SubER", "WER", "BLEU", "chrF", "TER"]);
    if let Some(lang) = language
        && ["zh", "ja", "ko"].contains(&lang)
    {
        cmd.args(["-l", lang]);
    }
    cmd.kill_on_drop(true);
    let status = cmd
        .status()
        .await
        .map_err(|e| format!("suber exec failed: {e}"))?;
    if !status.success() {
        return Err("suber returned non-zero".into());
    }
    Ok(())
}
