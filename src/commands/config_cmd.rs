use std::path::Path;

use crate::config::{self, Config};

pub fn handle(cmd: super::ConfigCommand, path: &Path, cfg: &Config) -> Result<(), String> {
    match cmd {
        super::ConfigCommand::Path => {
            println!("{}", path.display());
            Ok(())
        }
        super::ConfigCommand::Show => {
            println!("当前配置 ({}):\n", path.display());
            for (key, desc, options, show) in config::CONFIG_HELP {
                if !show {
                    continue;
                }
                let val = cfg.get(key).unwrap_or_default();
                let val_display = if key == &"api_key" && !val.is_empty() && val != "(not set)" {
                    format!("{}...", val.chars().take(4).collect::<String>())
                } else if val.is_empty() {
                    "(未设置)".to_string()
                } else if val.chars().count() > 14 {
                    let prefix: String = val.chars().take(11).collect();
                    format!("{prefix}...")
                } else {
                    val
                };
                println!("  {key:<22} {val_display:<16} {desc} [{options}]");
            }
            Ok(())
        }
        super::ConfigCommand::Get { key } => {
            let val = cfg.get(&key).ok_or_else(|| {
                let keys: Vec<&str> = config::CONFIG_HELP.iter().map(|(k, _, _, _)| *k).collect();
                format!("未知配置项: {key}\n可用配置项: {}", keys.join(", "))
            })?;
            println!("{val}");
            Ok(())
        }
        super::ConfigCommand::Set { key, value } => {
            let mut cfg = cfg.clone();
            cfg.set(&key, &value).map_err(|e| {
                if let Some((_, desc, options, _)) = config::CONFIG_HELP
                    .iter()
                    .find(|(k, _, _, _)| *k == key.as_str())
                {
                    format!("{e}\n  {key}: {desc} [{options}]")
                } else {
                    e
                }
            })?;
            cfg.save(path)?;
            println!("✓ {key} 已更新");
            Ok(())
        }
    }
}
