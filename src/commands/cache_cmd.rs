use crate::{cache, config::Config};

pub fn handle(cmd: super::CacheCommand, cfg: &Config) -> Result<(), String> {
    let cache_dir = cfg.cache_dir();
    match cmd {
        super::CacheCommand::Stats => {
            let stats = cache::cache_stats(&cache_dir);
            println!("缓存目录: {}", cache_dir.display());
            println!("文件数:  {}", stats.file_count);
            println!("总大小:  {}", format_bytes(stats.total_bytes));
            Ok(())
        }
        super::CacheCommand::Clean => {
            let (count, bytes) = cache::cache_clean(&cache_dir)?;
            println!("✓ 已删除 {count} 个文件，释放 {}", format_bytes(bytes));
            Ok(())
        }
        super::CacheCommand::Prune { days, max_mb } => {
            if days.is_none() && max_mb.is_none() {
                return Err("请指定 --days 和/或 --max-mb".into());
            }
            let max_bytes = max_mb.map(|mb| mb * 1024 * 1024);
            let (count, bytes) = cache::cache_prune(&cache_dir, days, max_bytes)?;
            println!("✓ 已删除 {count} 个文件，释放 {}", format_bytes(bytes));
            Ok(())
        }
    }
}

pub fn format_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if b >= GB {
        format!("{:.2} GB", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1} KB", b as f64 / KB as f64)
    } else {
        format!("{b} B")
    }
}
