//! nom-style build progress monitor.
//!
//! Parses nix's `--log-format internal-json` stderr output and renders a live
//! DAG-like display of derivation build status.

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use crossterm::{cursor, terminal};
use serde::Deserialize;
use yansi::Paint;

// --- Nix activity/result type constants ---

const ACT_COPY_PATH: u32 = 100;
const ACT_COPY_PATHS: u32 = 103;
const ACT_BUILDS: u32 = 104;
const ACT_BUILD: u32 = 105;
const ACT_SUBSTITUTE: u32 = 108;
const ACT_BUILD_WAITING: u32 = 111;

const RES_SET_PHASE: u32 = 104;
const RES_PROGRESS: u32 = 105;

/// Maximum number of activity lines to show.
const MAX_DISPLAY_LINES: usize = 12;

// --- JSON event parsing ---

#[derive(Debug, Deserialize)]
struct NixEvent {
    action: String,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    level: u32,
    #[serde(default)]
    text: String,
    #[serde(rename = "type", default)]
    activity_type: u32,
    #[serde(default)]
    fields: Vec<serde_json::Value>,
    #[serde(default)]
    msg: String,
}

// --- State tracking ---

#[derive(Debug, Clone)]
struct Activity {
    activity_type: u32,
    text: String,
    drv_name: Option<String>,
    phase: Option<String>,
    started_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrvState {
    Downloading,
    Succeeded,
    Failed,
}

/// Live build progress monitor that parses nix internal-json events.
pub struct BuildMonitor {
    activities: HashMap<u64, Activity>,
    /// Completed/failed derivation names for the summary.
    completed: Vec<(String, DrvState, std::time::Duration)>,
    /// Aggregate counters.
    build_done: u64,
    build_expected: u64,
    build_running: u64,
    build_failed: u64,
    download_done: u64,
    download_expected: u64,
    start_time: Instant,
    /// Number of lines we rendered last time (for clearing).
    last_render_lines: u16,
}

impl BuildMonitor {
    pub fn new() -> Self {
        Self {
            activities: HashMap::new(),
            completed: Vec::new(),
            build_done: 0,
            build_expected: 0,
            build_running: 0,
            build_failed: 0,
            download_done: 0,
            download_expected: 0,
            start_time: Instant::now(),
            last_render_lines: 0,
        }
    }

    /// Process a single line from nix's stderr.
    ///
    /// Lines starting with `@nix ` are parsed as JSON events.
    /// Other lines are ignored (or could be displayed as log output).
    pub fn process_line(&mut self, line: &str) {
        let Some(json_str) = line.strip_prefix("@nix ") else {
            return;
        };

        let Ok(event) = serde_json::from_str::<NixEvent>(json_str) else {
            return;
        };

        match event.action.as_str() {
            "start" => self.handle_start(&event),
            "stop" => self.handle_stop(&event),
            "result" => self.handle_result(&event),
            "msg" => self.handle_msg(&event),
            _ => {},
        }
    }

    fn handle_start(&mut self, event: &NixEvent) {
        let drv_name = match event.activity_type {
            ACT_BUILD | ACT_BUILD_WAITING => {
                // fields[0] = drv path, fields[1] = human name (sometimes)
                let name = if event.fields.len() > 1 {
                    event.fields[1].as_str().map(std::borrow::ToOwned::to_owned)
                } else {
                    None
                };
                name.or_else(|| extract_drv_name(&event.text))
            },
            ACT_SUBSTITUTE | ACT_COPY_PATH => extract_drv_name(&event.text),
            _ => None,
        };

        let activity = Activity {
            activity_type: event.activity_type,
            text: event.text.clone(),
            drv_name,
            phase: None,
            started_at: Instant::now(),
        };

        self.activities.insert(event.id, activity);
    }

    fn handle_stop(&mut self, event: &NixEvent) {
        if let Some(activity) = self.activities.remove(&event.id) {
            let elapsed = activity.started_at.elapsed();
            let name = activity.drv_name.unwrap_or_else(|| activity.text.clone());

            match activity.activity_type {
                ACT_BUILD => {
                    self.completed.push((name, DrvState::Succeeded, elapsed));
                },
                ACT_SUBSTITUTE | ACT_COPY_PATH => {
                    self.completed.push((name, DrvState::Downloading, elapsed));
                },
                _ => {},
            }
        }
    }

    fn handle_result(&mut self, event: &NixEvent) {
        match event.activity_type {
            RES_SET_PHASE => {
                if let Some(phase) = event.fields.first().and_then(|v| v.as_str()) {
                    if let Some(activity) = self.activities.get_mut(&event.id) {
                        activity.phase = Some(phase.to_owned());
                    }
                }
            },
            RES_PROGRESS if event.fields.len() >= 4 => {
                // fields: [done, expected, running, failed]
                if let Some(parent) = self.activities.get(&event.id) {
                    match parent.activity_type {
                        ACT_BUILDS => {
                            self.build_done = event.fields[0].as_u64().unwrap_or(0);
                            self.build_expected = event.fields[1].as_u64().unwrap_or(0);
                            self.build_running = event.fields[2].as_u64().unwrap_or(0);
                            self.build_failed = event.fields[3].as_u64().unwrap_or(0);
                        },
                        ACT_COPY_PATHS => {
                            self.download_done = event.fields[0].as_u64().unwrap_or(0);
                            self.download_expected = event.fields[1].as_u64().unwrap_or(0);
                        },
                        _ => {},
                    }
                }
            },
            _ => {},
        }
    }

    fn handle_msg(&mut self, event: &NixEvent) {
        // Error messages (level 0) from failed builds.
        if event.level == 0 && !event.msg.is_empty() {
            // Check if this references a build failure.
            if event.msg.contains("failed") {
                if let Some(name) = extract_drv_name(&event.msg) {
                    self.completed
                        .push((name, DrvState::Failed, self.start_time.elapsed()));
                }
            }
        }
    }

    /// Render the current state to stderr.
    pub fn render(&mut self) {
        let mut stderr = std::io::stderr();

        // Clear previous render.
        if self.last_render_lines > 0 {
            let _ = crossterm::execute!(
                stderr,
                cursor::MoveUp(self.last_render_lines),
                terminal::Clear(terminal::ClearType::FromCursorDown)
            );
        }

        let mut lines: Vec<String> = Vec::new();

        // Active builds.
        let mut active: Vec<&Activity> = self
            .activities
            .values()
            .filter(|a| a.activity_type == ACT_BUILD)
            .collect();
        active.sort_by_key(|a| std::cmp::Reverse(a.started_at));

        for activity in active.iter().take(MAX_DISPLAY_LINES / 2) {
            let name = activity.drv_name.as_deref().unwrap_or(&activity.text);
            let elapsed = format_duration(activity.started_at.elapsed());
            let phase = activity
                .phase
                .as_deref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default();
            lines.push(format!(
                "  {} {}{phase}  {elapsed}",
                "⏵".yellow().bold(),
                name.bold(),
            ));
        }

        // Waiting builds.
        let waiting: Vec<&Activity> = self
            .activities
            .values()
            .filter(|a| a.activity_type == ACT_BUILD_WAITING)
            .collect();
        for activity in waiting.iter().take(3) {
            let name = activity.drv_name.as_deref().unwrap_or(&activity.text);
            lines.push(format!("  {} {}", "⏸".blue(), name.dim()));
        }
        if waiting.len() > 3 {
            lines.push(format!(
                "  {} {} more waiting...",
                "⏸".blue(),
                waiting.len() - 3
            ));
        }

        // Active downloads.
        let downloads: Vec<&Activity> = self
            .activities
            .values()
            .filter(|a| a.activity_type == ACT_SUBSTITUTE || a.activity_type == ACT_COPY_PATH)
            .collect();
        for activity in downloads.iter().take(3) {
            let name = activity.drv_name.as_deref().unwrap_or(&activity.text);
            let elapsed = format_duration(activity.started_at.elapsed());
            lines.push(format!("  {} {}  {elapsed}", "↓".cyan().bold(), name,));
        }
        if downloads.len() > 3 {
            lines.push(format!(
                "  {} {} more downloading...",
                "↓".cyan(),
                downloads.len() - 3
            ));
        }

        // Recent completions (last 3).
        let recent_completed: Vec<_> = self.completed.iter().rev().take(3).collect();
        for (name, state, elapsed) in recent_completed.into_iter().rev() {
            let dur = format_duration(*elapsed);
            match state {
                DrvState::Succeeded => {
                    lines.push(format!("  {} {}  {dur}", "✔".green(), name.dim()));
                },
                DrvState::Failed => {
                    lines.push(format!("  {} {}", "✘".red().bold(), name.red()));
                },
                DrvState::Downloading => {
                    lines.push(format!("  {} {}  {dur}", "↓ ✔".green(), name.dim()));
                },
            }
        }

        // Summary line.
        let elapsed = format_duration(self.start_time.elapsed());
        let mut summary_parts = Vec::new();

        if self.build_expected > 0 || self.build_done > 0 {
            summary_parts.push(format!("{}/{} built", self.build_done, self.build_expected));
        }
        if self.build_running > 0 {
            summary_parts.push(format!("{} building", self.build_running));
        }
        if self.download_done > 0 || self.download_expected > 0 {
            summary_parts.push(format!(
                "{}/{} downloaded",
                self.download_done, self.download_expected
            ));
        }
        if self.build_failed > 0 {
            summary_parts.push(format!("{} {}", self.build_failed, "failed".red()));
        }

        if !summary_parts.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "  {}  {}  {} {elapsed}",
                "∑".bold(),
                summary_parts.join("  "),
                "⏱".dim(),
            ));
        }

        // Write lines.
        for line in &lines {
            let _ = writeln!(stderr, "{line}");
        }

        self.last_render_lines = lines.len() as u16;
        let _ = stderr.flush();
    }

    /// Clear the rendered output (call when done).
    pub fn clear_display(&self) {
        if self.last_render_lines > 0 {
            let mut stderr = std::io::stderr();
            let _ = crossterm::execute!(
                stderr,
                cursor::MoveUp(self.last_render_lines),
                terminal::Clear(terminal::ClearType::FromCursorDown)
            );
        }
    }

    /// Print a final summary after the build completes.
    pub fn finish(&mut self) {
        self.clear_display();

        let elapsed = format_duration(self.start_time.elapsed());
        let mut stderr = std::io::stderr();

        // Show all failures.
        for (name, state, _) in &self.completed {
            if *state == DrvState::Failed {
                let _ = writeln!(stderr, "  {} {}", "✘".red().bold(), name.red());
            }
        }

        // Final summary.
        let mut parts = Vec::new();
        if self.build_done > 0 {
            parts.push(format!("{} {}", self.build_done, "built".green()));
        }
        if self.download_done > 0 {
            parts.push(format!("{} {}", self.download_done, "downloaded".cyan()));
        }
        if self.build_failed > 0 {
            parts.push(format!("{} {}", self.build_failed, "failed".red()));
        }

        if !parts.is_empty() {
            let _ = writeln!(
                stderr,
                "  {} {}  {} {elapsed}",
                "∑".bold(),
                parts.join("  "),
                "⏱".dim(),
            );
        }
    }
}

impl Default for BuildMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract a human-readable derivation name from a nix text/path.
///
/// `/nix/store/abc123-hello-2.12.1.drv` → `hello-2.12.1`
/// `building '/nix/store/abc123-hello-2.12.1.drv'` → `hello-2.12.1`
fn extract_drv_name(text: &str) -> Option<String> {
    // Find a store path in the text.
    let start = text.find("/nix/store/")?;
    let path = &text[start..];
    let end = path.find(['\'', '"', ' ', ')']);
    let path = match end {
        Some(e) => &path[..e],
        None => path,
    };

    // Extract basename and strip hash prefix.
    let basename = path.rsplit('/').next()?;
    let name = basename.split_once('-').map(|(_, rest)| rest)?;
    // Strip .drv suffix if present.
    let name = name.strip_suffix(".drv").unwrap_or(name);
    Some(name.to_owned())
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_drv_name_from_path() {
        assert_eq!(
            extract_drv_name("/nix/store/abc123-hello-2.12.1.drv"),
            Some("hello-2.12.1".to_owned())
        );
    }

    #[test]
    fn extract_drv_name_from_building_text() {
        assert_eq!(
            extract_drv_name("building '/nix/store/abc123-firefox-131.0.drv'"),
            Some("firefox-131.0".to_owned())
        );
    }

    #[test]
    fn extract_drv_name_store_path_no_drv() {
        assert_eq!(
            extract_drv_name("/nix/store/xyz789-glibc-2.39"),
            Some("glibc-2.39".to_owned())
        );
    }

    #[test]
    fn parse_start_event() {
        let mut monitor = BuildMonitor::new();
        monitor.process_line(
            r#"@nix {"action":"start","id":42,"level":0,"text":"building '/nix/store/abc-hello-1.0.drv'","type":105,"parent":0,"fields":[]}"#,
        );
        assert!(monitor.activities.contains_key(&42));
        assert_eq!(monitor.activities[&42].activity_type, ACT_BUILD);
    }

    #[test]
    fn parse_stop_event() {
        let mut monitor = BuildMonitor::new();
        monitor.process_line(
            r#"@nix {"action":"start","id":42,"level":0,"text":"building '/nix/store/abc-hello-1.0.drv'","type":105,"parent":0,"fields":[]}"#,
        );
        monitor.process_line(r#"@nix {"action":"stop","id":42}"#);
        assert!(!monitor.activities.contains_key(&42));
        assert_eq!(monitor.completed.len(), 1);
        assert_eq!(monitor.completed[0].1, DrvState::Succeeded);
    }

    #[test]
    fn parse_phase_result() {
        let mut monitor = BuildMonitor::new();
        monitor.process_line(
            r#"@nix {"action":"start","id":42,"level":0,"text":"building","type":105,"parent":0,"fields":[]}"#,
        );
        monitor.process_line(
            r#"@nix {"action":"result","id":42,"type":104,"fields":["configuring"]}"#,
        );
        assert_eq!(
            monitor.activities[&42].phase.as_deref(),
            Some("configuring")
        );
    }

    #[test]
    fn parse_progress_result() {
        let mut monitor = BuildMonitor::new();
        monitor.process_line(
            r#"@nix {"action":"start","id":1,"level":0,"text":"","type":104,"parent":0,"fields":[]}"#,
        );
        monitor.process_line(r#"@nix {"action":"result","id":1,"type":105,"fields":[3,8,2,0]}"#);
        assert_eq!(monitor.build_done, 3);
        assert_eq!(monitor.build_expected, 8);
        assert_eq!(monitor.build_running, 2);
    }

    #[test]
    fn ignores_non_nix_lines() {
        let mut monitor = BuildMonitor::new();
        monitor.process_line("some random output");
        monitor.process_line("");
        monitor.process_line("@nix {invalid json}");
        assert!(monitor.activities.is_empty());
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(std::time::Duration::from_secs(45)), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(std::time::Duration::from_secs(125)), "2m5s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(
            format_duration(std::time::Duration::from_secs(3665)),
            "1h1m"
        );
    }
}
