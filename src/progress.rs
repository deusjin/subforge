use std::io::{self, IsTerminal, Write};
use std::time::Instant;

use crate::logging::{self, Level};

pub fn render_bar(label: &str, current: usize, total: usize, width: usize) -> String {
    let total = total.max(1);
    let current = current.min(total);
    let filled = current * width / total;
    let empty = width.saturating_sub(filled);
    let percent = current * 100 / total;
    format!(
        "{label} [{}{}] {current}/{total} {percent:>3}%",
        "#".repeat(filled),
        "-".repeat(empty),
    )
}

pub fn format_elapsed(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

pub struct ProgressBar {
    label: String,
    total: usize,
    current: usize,
    tty: bool,
    active: bool,
}

impl ProgressBar {
    pub fn new(label: impl Into<String>, total: usize) -> Self {
        let active = logging::level() != Level::Quiet && total > 0;
        Self {
            label: label.into(),
            total,
            current: 0,
            tty: io::stderr().is_terminal(),
            active,
        }
    }

    pub fn inc(&mut self, delta: usize) {
        self.current = (self.current + delta).min(self.total);
        self.draw();
    }

    pub fn finish(&mut self) {
        if self.current < self.total {
            self.current = self.total;
            self.draw();
        }
        if self.active && self.tty {
            let _ = writeln!(io::stderr());
        }
    }

    fn draw(&self) {
        if !self.active {
            return;
        }
        let line = render_bar(&self.label, self.current, self.total, 28);
        if self.tty {
            let _ = write!(io::stderr(), "\r{line}");
            let _ = io::stderr().flush();
        } else {
            let _ = writeln!(io::stderr(), "{line}");
        }
    }
}

pub struct Heartbeat {
    label: String,
    started: Instant,
    tick: usize,
    tty: bool,
    active: bool,
}

impl Heartbeat {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            started: Instant::now(),
            tick: 0,
            tty: io::stderr().is_terminal(),
            active: logging::level() != Level::Quiet,
        }
    }

    pub fn tick(&mut self) {
        if !self.active {
            return;
        }
        self.tick += 1;
        let spinner = ["|", "/", "-", "\\"][self.tick % 4];
        let elapsed = self.started.elapsed().as_secs();
        let line = format!("{} {spinner} elapsed {}s", self.label, elapsed);
        if self.tty {
            let _ = write!(io::stderr(), "\r{line}");
            let _ = io::stderr().flush();
        } else {
            let _ = writeln!(io::stderr(), "{line}");
        }
    }

    pub fn finish(&self) {
        if self.active && self.tty {
            let _ = writeln!(io::stderr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_bar_shows_counts_and_percent() {
        assert_eq!(
            render_bar("Translating", 3, 10, 10),
            "Translating [###-------] 3/10  30%"
        );
    }

    #[test]
    fn render_bar_clamps_current() {
        assert_eq!(render_bar("Done", 12, 10, 5), "Done [#####] 10/10 100%");
    }

    #[test]
    fn format_elapsed_uses_seconds_minutes_and_hours() {
        assert_eq!(format_elapsed(std::time::Duration::from_secs(7)), "7s");
        assert_eq!(format_elapsed(std::time::Duration::from_secs(65)), "1m 5s");
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(3661)),
            "1h 1m 1s"
        );
    }
}
