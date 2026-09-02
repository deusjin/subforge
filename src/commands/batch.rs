use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::task::JoinSet;

use crate::cli::BatchCommon;
use crate::config::Config;
use crate::progress::format_elapsed;
use crate::{commands::process, synthesize, util};

#[derive(Debug, Clone)]
struct DiscoveredInput {
    path: PathBuf,
    root: PathBuf,
}

#[derive(Debug, Clone)]
struct BatchPlan {
    tm_dir: Option<PathBuf>,
    items: Vec<ItemPlan>,
}

#[derive(Debug, Clone)]
struct ItemPlan {
    input: PathBuf,
    output_arg: Option<PathBuf>,
    outputs: Vec<PathBuf>,
    skipped: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum ItemStatus {
    Planned,
    Success,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
struct ItemReport {
    input: String,
    status: ItemStatus,
    outputs: Vec<String>,
    error: Option<String>,
    elapsed_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
struct BatchReport {
    tm_dir: Option<String>,
    total: usize,
    succeeded: usize,
    skipped: usize,
    failed: usize,
    items: Vec<ItemReport>,
}

pub async fn run(
    common: BatchCommon,
    opts: process::Options,
    cfg: &Config,
    config_path: Option<&Path>,
) -> Result<(), String> {
    let output_root = match &common.output {
        Some(output) => {
            let path = abs_path(Path::new(output))?;
            if path.exists() && !path.is_dir() {
                return Err(format!(
                    "batch output must be a directory: {}",
                    display_path(&path)
                ));
            }
            Some(path)
        }
        None => None,
    };

    let mut cfg = cfg.clone();
    let discovered = discover_inputs(&common.inputs, common.recursive)?;
    if cfg.tm_dir.is_empty() && !discovered.is_empty() {
        let tm_dir = shared_tm_dir(output_root.as_deref(), &discovered);
        cfg.tm_dir = tm_dir.to_string_lossy().into_owned();
    }

    let plan = build_plan(
        &discovered,
        output_root.as_deref(),
        common.overwrite,
        &opts,
        &cfg,
    )?;

    if common.dry_run {
        print_dry_run(&plan);
        let reports = plan
            .items
            .iter()
            .map(|item| ItemReport {
                input: display_path(&item.input),
                status: if item.skipped {
                    ItemStatus::Skipped
                } else {
                    ItemStatus::Planned
                },
                outputs: item.outputs.iter().map(|p| display_path(p)).collect(),
                error: None,
                elapsed_secs: 0.0,
            })
            .collect::<Vec<_>>();
        let report = summarize(plan.tm_dir.as_deref(), reports);
        print_summary(&report);
        if let Some(path) = common.report.as_deref() {
            write_report(Path::new(path), &report)?;
        }
        return Ok(());
    }

    let started = Instant::now();
    let mut reports = Vec::new();
    let pending = plan.items.iter().filter(|item| !item.skipped).count();

    for item in plan.items.iter().filter(|item| item.skipped) {
        println!("skip {}", display_path(&item.input));
        reports.push(ItemReport {
            input: display_path(&item.input),
            status: ItemStatus::Skipped,
            outputs: item.outputs.iter().map(|p| display_path(p)).collect(),
            error: None,
            elapsed_secs: 0.0,
        });
    }

    if common.jobs == 1 {
        for item in plan.items.iter().filter(|item| !item.skipped).cloned() {
            reports.push(
                run_one(
                    item,
                    cfg.clone(),
                    opts.clone(),
                    config_path.map(PathBuf::from),
                )
                .await,
            );
        }
    } else {
        let cfg = Arc::new(cfg);
        let opts = Arc::new(opts);
        let config_path = config_path.map(PathBuf::from);
        let mut set = JoinSet::new();
        let mut running = 0usize;
        let mut iter = plan.items.into_iter().filter(|item| !item.skipped);

        loop {
            while running < common.jobs {
                let Some(item) = iter.next() else {
                    break;
                };
                running += 1;
                let cfg = Arc::clone(&cfg);
                let opts = Arc::clone(&opts);
                let config_path = config_path.clone();
                set.spawn(async move {
                    run_one(item, (*cfg).clone(), (*opts).clone(), config_path).await
                });
            }

            if running == 0 {
                break;
            }

            match set.join_next().await {
                Some(Ok(report)) => reports.push(report),
                Some(Err(e)) => reports.push(ItemReport {
                    input: "<task>".into(),
                    status: ItemStatus::Failed,
                    outputs: Vec::new(),
                    error: Some(format!("batch worker failed: {e}")),
                    elapsed_secs: 0.0,
                }),
                None => break,
            }
            running -= 1;
        }
    }

    reports.sort_by(|a, b| a.input.cmp(&b.input));
    let report = summarize(plan.tm_dir.as_deref(), reports);
    print_summary(&report);
    println!(
        "Batch elapsed: {} (processed {pending} item{})",
        format_elapsed(started.elapsed()),
        if pending == 1 { "" } else { "s" }
    );

    if let Some(path) = common.report.as_deref() {
        write_report(Path::new(path), &report)?;
    }

    if report.failed > 0 {
        Err(format!(
            "batch completed with {} failed item{}",
            report.failed,
            if report.failed == 1 { "" } else { "s" }
        ))
    } else {
        Ok(())
    }
}

async fn run_one(
    item: ItemPlan,
    cfg: Config,
    opts: process::Options,
    config_path: Option<PathBuf>,
) -> ItemReport {
    println!("run {}", display_path(&item.input));
    let started = Instant::now();
    let result = async {
        let input = path_str(&item.input, "batch input")?;
        let output = match &item.output_arg {
            Some(path) => Some(path_str(path, "batch output")?),
            None => None,
        };
        process::run_with_config_path(input, output, opts, &cfg, config_path.as_deref()).await
    }
    .await;

    let elapsed = started.elapsed().as_secs_f64();
    match result {
        Ok(path) => ItemReport {
            input: display_path(&item.input),
            status: ItemStatus::Success,
            outputs: merge_outputs(&item.outputs, path),
            error: None,
            elapsed_secs: elapsed,
        },
        Err(error) => ItemReport {
            input: display_path(&item.input),
            status: ItemStatus::Failed,
            outputs: item.outputs.iter().map(|p| display_path(p)).collect(),
            error: Some(error),
            elapsed_secs: elapsed,
        },
    }
}

fn discover_inputs(inputs: &[String], recursive: bool) -> Result<Vec<DiscoveredInput>, String> {
    let mut errors = Vec::new();
    let mut found = Vec::new();

    for raw in inputs {
        let input = abs_path(Path::new(raw))?;
        if !input.exists() {
            errors.push(format!("input not found: {}", display_path(&input)));
            continue;
        }

        if input.is_file() {
            if util::is_video_file(&input) {
                let root = input.parent().unwrap_or(Path::new(".")).to_path_buf();
                found.push(DiscoveredInput { path: input, root });
            } else {
                errors.push(format!(
                    "explicit input is not a video: {}",
                    display_path(&input)
                ));
            }
            continue;
        }

        if input.is_dir() {
            collect_dir(&input, &input, recursive, &mut found)?;
            continue;
        }

        errors.push(format!("unsupported input type: {}", display_path(&input)));
    }

    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }

    let mut unique = BTreeMap::new();
    for item in found {
        let key = item
            .path
            .canonicalize()
            .unwrap_or_else(|_| item.path.clone());
        unique.entry(key).or_insert(item);
    }
    let mut items = unique.into_values().collect::<Vec<_>>();
    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(items)
}

fn collect_dir(
    dir: &Path,
    root: &Path,
    recursive: bool,
    found: &mut Vec<DiscoveredInput>,
) -> Result<(), String> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| format!("read directory {}: {e}", display_path(dir)))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read directory {}: {e}", display_path(dir)))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_file() {
            if util::is_video_file(&path) {
                found.push(DiscoveredInput {
                    path,
                    root: root.to_path_buf(),
                });
            }
        } else if recursive && path.is_dir() {
            collect_dir(&path, root, recursive, found)?;
        }
    }
    Ok(())
}

fn build_plan(
    inputs: &[DiscoveredInput],
    output_root: Option<&Path>,
    overwrite: bool,
    opts: &process::Options,
    cfg: &Config,
) -> Result<BatchPlan, String> {
    let mut items = Vec::new();
    let mut seen_outputs = BTreeMap::<PathBuf, PathBuf>::new();

    for input in inputs {
        let outputs = predicted_outputs(input, output_root, opts);
        for output in &outputs {
            let key = abs_path(output)?;
            if let Some(previous) = seen_outputs.insert(key, input.path.clone()) {
                return Err(format!(
                    "output collision: {} would be written by both {} and {}",
                    display_path(output),
                    display_path(&previous),
                    display_path(&input.path)
                ));
            }
        }
        let output_arg = output_root.map(|_| outputs[0].clone());
        let skipped = !overwrite && outputs.iter().all(|path| path.exists());
        items.push(ItemPlan {
            input: input.path.clone(),
            output_arg,
            outputs,
            skipped,
        });
    }

    Ok(BatchPlan {
        tm_dir: if cfg.tm_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(&cfg.tm_dir))
        },
        items,
    })
}

fn predicted_outputs(
    input: &DiscoveredInput,
    output_root: Option<&Path>,
    opts: &process::Options,
) -> Vec<PathBuf> {
    let dir = match output_root {
        Some(root) => {
            let rel = input.path.strip_prefix(&input.root).unwrap_or(&input.path);
            match rel.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => root.join(parent),
                _ => root.to_path_buf(),
            }
        }
        None => input.path.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };

    let stem = input.path.file_stem().unwrap_or_default().to_string_lossy();
    let final_path = if opts.no_synthesize {
        dir.join(util::translated_output_name(&stem, "srt"))
    } else {
        let ext = synthesized_default_extension(&opts.synth, &input.path);
        dir.join(format!("{stem}_captioned.{ext}"))
    };

    let mut outputs = vec![final_path.clone()];
    if !opts.no_synthesize && opts.synth.mode == synthesize::Mode::Both {
        outputs.push(insert_marker_before_ext(&final_path, "_softsub"));
    }
    outputs
}

fn synthesized_default_extension(opts: &synthesize::Options, input: &Path) -> String {
    match opts.mode {
        synthesize::Mode::Soft => "mkv".into(),
        _ => input
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    }
}

fn insert_marker_before_ext(path: &Path, marker: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}{marker}.{ext}"))
}

fn shared_tm_dir(output_root: Option<&Path>, inputs: &[DiscoveredInput]) -> PathBuf {
    match output_root {
        Some(root) => root.join(".subforge-tm"),
        None => common_root(inputs).join(".subforge-tm"),
    }
}

fn common_root(inputs: &[DiscoveredInput]) -> PathBuf {
    let mut candidate = inputs
        .first()
        .map(|input| input.root.clone())
        .unwrap_or_else(|| PathBuf::from("."));

    while !inputs
        .iter()
        .all(|input| input.root.starts_with(&candidate))
    {
        if !candidate.pop() {
            return PathBuf::from(".");
        }
    }
    candidate
}

fn print_dry_run(plan: &BatchPlan) {
    if let Some(tm_dir) = &plan.tm_dir {
        println!("TM: {}", display_path(tm_dir));
    }
    if plan.items.is_empty() {
        println!("No videos found.");
        return;
    }
    for item in &plan.items {
        let action = if item.skipped { "SKIP" } else { "RUN" };
        println!("{action} {}", display_path(&item.input));
        for output in &item.outputs {
            println!("  -> {}", display_path(output));
        }
    }
}

fn summarize(tm_dir: Option<&Path>, items: Vec<ItemReport>) -> BatchReport {
    let succeeded = items
        .iter()
        .filter(|item| item.status == ItemStatus::Success)
        .count();
    let skipped = items
        .iter()
        .filter(|item| item.status == ItemStatus::Skipped)
        .count();
    let failed = items
        .iter()
        .filter(|item| item.status == ItemStatus::Failed)
        .count();
    BatchReport {
        tm_dir: tm_dir.map(display_path),
        total: items.len(),
        succeeded,
        skipped,
        failed,
        items,
    }
}

fn print_summary(report: &BatchReport) {
    let planned = report
        .items
        .iter()
        .filter(|item| item.status == ItemStatus::Planned)
        .count();
    if planned > 0 {
        println!(
            "Batch summary: {planned} planned, {} skipped, {} failed ({} total)",
            report.skipped, report.failed, report.total
        );
    } else {
        println!(
            "Batch summary: {} succeeded, {} skipped, {} failed ({} total)",
            report.succeeded, report.skipped, report.failed, report.total
        );
    }
    for item in report
        .items
        .iter()
        .filter(|item| item.status == ItemStatus::Failed)
    {
        let error = item.error.as_deref().unwrap_or("unknown error");
        println!("failed: {}: {error}", item.input);
    }
}

fn write_report(path: &Path, report: &BatchReport) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create report dir {}: {e}", display_path(parent)))?;
    }
    let json = serde_json::to_string_pretty(report).map_err(|e| format!("encode report: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write report {}: {e}", display_path(path)))
}

fn merge_outputs(predicted: &[PathBuf], actual: PathBuf) -> Vec<String> {
    let mut set = BTreeSet::new();
    set.insert(display_path(&actual));
    for path in predicted {
        set.insert(display_path(path));
    }
    set.into_iter().collect()
}

fn path_str<'a>(path: &'a Path, label: &str) -> Result<&'a str, String> {
    path.to_str()
        .ok_or_else(|| format!("{label} path is not valid UTF-8: {}", display_path(path)))
}

fn display_path(path: &Path) -> String {
    if !path.is_absolute() {
        return path.to_string_lossy().into_owned();
    }

    if let Ok(cwd) = std::env::current_dir()
        && let Ok(relative) = path.strip_prefix(cwd)
    {
        return if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
    }

    // Absolute paths outside the current project can contain usernames,
    // dataset names, and other host-specific details. Keep them opaque in
    // logs and JSON reports; the real path is still used internally.
    "<external-path>".into()
}

fn abs_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| format!("read current directory: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"fake").unwrap();
    }

    fn opts_process(mode: synthesize::Mode) -> process::Options {
        process::Options {
            no_synthesize: false,
            no_cache: true,
            keep_intermediate: false,
            synth: synthesize::Options {
                mode,
                ..Default::default()
            },
        }
    }

    fn opts_translate() -> process::Options {
        process::Options {
            no_synthesize: true,
            no_cache: true,
            keep_intermediate: false,
            synth: synthesize::Options::default(),
        }
    }

    #[test]
    fn discover_directory_respects_recursive_flag() {
        let dir = tempdir().unwrap();
        touch(&dir.path().join("a.mp4"));
        touch(&dir.path().join("b.srt"));
        touch(&dir.path().join("nested").join("c.mkv"));

        let flat = discover_inputs(&[dir.path().display().to_string()], false).unwrap();
        assert_eq!(flat.len(), 1);
        assert!(flat[0].path.ends_with("a.mp4"));

        let recursive = discover_inputs(&[dir.path().display().to_string()], true).unwrap();
        assert_eq!(recursive.len(), 2);
    }

    #[test]
    fn explicit_non_video_is_an_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        touch(&file);

        let err = discover_inputs(&[file.display().to_string()], false).unwrap_err();
        assert!(err.contains("not a video"));
    }

    #[test]
    fn output_root_preserves_relative_structure() {
        let dir = tempdir().unwrap();
        let out = tempdir().unwrap();
        let input = dir.path().join("season").join("ep1.mp4");
        touch(&input);
        let discovered = DiscoveredInput {
            path: input,
            root: dir.path().to_path_buf(),
        };

        let outputs = predicted_outputs(&discovered, Some(out.path()), &opts_translate());
        assert_eq!(
            outputs[0],
            out.path().join("season").join("ep1_translated.srt")
        );
    }

    #[test]
    fn output_collisions_are_rejected() {
        let dir = tempdir().unwrap();
        let out = tempdir().unwrap();
        let one = dir.path().join("one").join("same.mp4");
        let two = dir.path().join("two").join("same.mp4");
        touch(&one);
        touch(&two);
        let inputs = vec![
            DiscoveredInput {
                path: one,
                root: dir.path().join("one"),
            },
            DiscoveredInput {
                path: two,
                root: dir.path().join("two"),
            },
        ];

        let err = build_plan(
            &inputs,
            Some(out.path()),
            false,
            &opts_translate(),
            &Config::default(),
        )
        .unwrap_err();
        assert!(err.contains("output collision"));
    }

    #[test]
    fn both_mode_skip_requires_both_outputs() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("demo.mp4");
        touch(&input);
        let item = DiscoveredInput {
            path: input,
            root: dir.path().to_path_buf(),
        };
        let opts = opts_process(synthesize::Mode::Both);
        let outputs = predicted_outputs(&item, None, &opts);
        assert_eq!(outputs.len(), 2);

        touch(&outputs[0]);
        let plan = build_plan(
            std::slice::from_ref(&item),
            None,
            false,
            &opts,
            &Config::default(),
        )
        .unwrap();
        assert!(!plan.items[0].skipped);

        touch(&outputs[1]);
        let plan = build_plan(&[item], None, false, &opts, &Config::default()).unwrap();
        assert!(plan.items[0].skipped);
    }

    #[test]
    fn shared_tm_prefers_output_root() {
        let dir = tempdir().unwrap();
        let out = tempdir().unwrap();
        let input = DiscoveredInput {
            path: dir.path().join("demo.mp4"),
            root: dir.path().to_path_buf(),
        };

        assert_eq!(
            shared_tm_dir(Some(out.path()), std::slice::from_ref(&input)),
            out.path().join(".subforge-tm")
        );
        assert_eq!(
            shared_tm_dir(None, &[input]),
            dir.path().join(".subforge-tm")
        );
    }

    #[test]
    fn report_serializes_items() {
        let report = summarize(
            Some(Path::new("/tmp/tm")),
            vec![ItemReport {
                input: "/tmp/a.mp4".into(),
                status: ItemStatus::Success,
                outputs: vec!["/tmp/a.srt".into()],
                error: None,
                elapsed_secs: 1.25,
            }],
        );

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(json.contains("\"succeeded\":1"));
    }

    #[test]
    fn display_path_hides_external_absolute_paths() {
        let external = std::env::temp_dir()
            .join("subforge-privacy-test")
            .join("video.mp4");
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        if external.is_absolute() && external.strip_prefix(cwd).is_err() {
            assert_eq!(display_path(&external), "<external-path>");
        }
    }
}
