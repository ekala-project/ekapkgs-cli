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
    parent: u64,
    #[serde(default)]
    fields: Vec<serde_json::Value>,
    #[serde(default)]
    msg: String,
}

// --- State tracking ---

#[derive(Debug, Clone)]
struct Activity {
    activity_type: u32,
    #[allow(dead_code)]
    parent: u64,
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
    /// Last rendered frame content (for diff-based redraw suppression).
    last_frame: String,
    /// Error/warning messages from nix (level 0 and 1 msg events).
    error_messages: Vec<String>,
    /// Whether we've started rendering.
    has_rendered: bool,
    /// Whether we've seen any actual build activity (not just downloads).
    /// Set to true when we see ACT_BUILD start or build_expected > 0.
    saw_builds: bool,
    /// Set to true once nix reports expected counts, so we know if there's
    /// actually work to display.
    saw_expected: bool,
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
            error_messages: Vec::new(),
            last_render_lines: 0,
            last_frame: String::new(),
            has_rendered: false,
            saw_builds: false,
            saw_expected: false,
        }
    }

    /// Return collected error/warning messages from nix.
    pub fn error_messages(&self) -> &[String] {
        &self.error_messages
    }

    /// Whether the monitor has determined there is work worth displaying.
    ///
    /// Returns false when everything is cached (0 expected builds) or when
    /// nix hasn't reported any build activity yet.
    fn should_render(&self) -> bool {
        // If we've already rendered, keep rendering.
        if self.has_rendered {
            return true;
        }
        // If nix reported expected counts and there are actual builds, render.
        if self.saw_expected && self.build_expected > 0 {
            return true;
        }
        // If we see any active build derivation, render.
        if self.saw_builds {
            return true;
        }
        false
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
            parent: event.parent,
            text: event.text.clone(),
            drv_name,
            phase: None,
            started_at: Instant::now(),
        };

        if event.activity_type == ACT_BUILD {
            self.saw_builds = true;
        }

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
                            self.saw_expected = true;
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
        if event.msg.is_empty() {
            return;
        }

        // Level 0 = error, level 1 = warning.
        if event.level <= 1 {
            self.error_messages.push(event.msg.clone());
        }

        // Track build failures for the DAG display.
        if event.level == 0 && event.msg.contains("failed") {
            if let Some(name) = extract_drv_name(&event.msg) {
                self.completed
                    .push((name, DrvState::Failed, self.start_time.elapsed()));
            }
        }
    }

    /// Render the current state to stderr with nom-style tree display.
    ///
    /// Suppresses rendering when there are no actual builds to show
    /// (e.g., fully cached closures where nix reports 0 expected builds).
    pub fn render(&mut self) {
        if !self.should_render() {
            return;
        }
        self.has_rendered = true;

        let mut lines: Vec<String> = Vec::new();

        // ━━━ Build dependency tree ━━━
        let mut active_builds: Vec<(&u64, &Activity)> = self
            .activities
            .iter()
            .filter(|(_, a)| a.activity_type == ACT_BUILD)
            .collect();
        active_builds.sort_by_key(|(_, a)| a.started_at);

        let waiting_builds: Vec<(&u64, &Activity)> = self
            .activities
            .iter()
            .filter(|(_, a)| a.activity_type == ACT_BUILD_WAITING)
            .collect();

        if !active_builds.is_empty() || !waiting_builds.is_empty() {
            let total_tree_items = active_builds.len() + waiting_builds.len();

            for (idx, (_id, activity)) in active_builds.iter().enumerate() {
                let name = activity.drv_name.as_deref().unwrap_or(&activity.text);
                let elapsed = format_duration(activity.started_at.elapsed());
                let phase = activity
                    .phase
                    .as_deref()
                    .map(|p| format!(" {}", p.dim()))
                    .unwrap_or_default();
                let is_last = idx == total_tree_items - 1 && waiting_builds.is_empty();
                let branch = if is_last { "┗" } else { "┣" };
                lines.push(format!(
                    " {} {} {}{}  {}",
                    branch.dim(),
                    "⏵".yellow().bold(),
                    name.yellow().bold(),
                    phase,
                    elapsed.dim(),
                ));
            }

            for (idx, (_id, activity)) in waiting_builds.iter().enumerate() {
                let name = activity.drv_name.as_deref().unwrap_or(&activity.text);
                let is_last = idx == waiting_builds.len() - 1;
                let branch = if is_last { "┗" } else { "┣" };
                if idx < 3 {
                    lines.push(format!(" {} {} {}", branch.dim(), "⏸".blue(), name.dim(),));
                } else if idx == 3 {
                    let remaining = waiting_builds.len() - 3;
                    lines.push(format!(
                        " {} {} {}",
                        "┗".dim(),
                        "⏸".blue(),
                        format!("…{remaining} more queued").dim(),
                    ));
                    break;
                }
            }
        }

        // ━━━ Downloads ━━━
        let mut downloads: Vec<&Activity> = self
            .activities
            .values()
            .filter(|a| a.activity_type == ACT_SUBSTITUTE || a.activity_type == ACT_COPY_PATH)
            .collect();
        downloads.sort_by_key(|a| a.started_at);

        if !downloads.is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            for (idx, activity) in downloads.iter().enumerate() {
                let name = activity.drv_name.as_deref().unwrap_or(&activity.text);
                let elapsed = format_duration(activity.started_at.elapsed());
                if idx < 4 {
                    let is_last = idx == downloads.len() - 1 || idx == 3;
                    let branch = if is_last { "┗" } else { "┣" };
                    lines.push(format!(
                        " {} {} {}  {}",
                        branch.dim(),
                        "↓".cyan().bold(),
                        name.cyan(),
                        elapsed.dim(),
                    ));
                } else if idx == 4 {
                    let remaining = downloads.len() - 4;
                    lines.push(format!(
                        " {} {} {}",
                        "┗".dim(),
                        "↓".cyan(),
                        format!("…{remaining} more").dim(),
                    ));
                    break;
                }
            }
        }

        // ━━━ Summary table ━━━
        lines.push(String::new());
        let elapsed = format_duration(self.start_time.elapsed());
        let sep = "━".repeat(42).dim().to_string();
        lines.push(format!(" {sep}"));

        // Builds row
        let build_planned = self
            .build_expected
            .saturating_sub(self.build_done + self.build_running + self.build_failed);
        lines.push(format!(
            "  {} {} {}  {} {}  {} {}  {} {}",
            "Builds   ".bold(),
            "⏵".yellow().bold(),
            self.build_running.to_string().yellow().bold(),
            "✔".green(),
            self.build_done.to_string().green(),
            "⏸".blue(),
            build_planned.to_string().blue(),
            "∑".dim(),
            self.build_expected.to_string().dim(),
        ));

        // Downloads row
        let dl_running = self.download_expected.saturating_sub(self.download_done);
        lines.push(format!(
            "  {} {} {}  {} {}  {} {}",
            "Downloads".bold(),
            "↓".cyan().bold(),
            dl_running.to_string().cyan().bold(),
            "✔".green(),
            self.download_done.to_string().green(),
            "∑".dim(),
            self.download_expected.to_string().dim(),
        ));

        // Failures row (only if any)
        if self.build_failed > 0 {
            lines.push(format!(
                "  {} {} {}",
                "Errors   ".bold(),
                "✘".red().bold(),
                self.build_failed.to_string().red().bold(),
            ));
        }

        // Time row
        lines.push(format!(
            "  {} {} {}",
            "Time     ".bold(),
            "⏱".dim(),
            elapsed,
        ));

        lines.push(format!(" {sep}"));

        // Buffer the entire frame.
        let mut buf = String::new();
        for line in &lines {
            buf.push_str(line);
            buf.push('\n');
        }

        // Skip redraw if the frame content hasn't changed.
        if buf == self.last_frame {
            return;
        }

        let mut stderr = std::io::stderr();

        // Hide cursor, clear previous frame, write new frame, show cursor.
        let _ = crossterm::execute!(stderr, cursor::Hide);
        if self.last_render_lines > 0 {
            let _ = crossterm::execute!(
                stderr,
                cursor::MoveUp(self.last_render_lines),
                terminal::Clear(terminal::ClearType::FromCursorDown)
            );
        }

        let _ = stderr.write_all(buf.as_bytes());
        self.last_render_lines = lines.len() as u16;
        self.last_frame = buf;
        let _ = crossterm::execute!(stderr, cursor::Show);
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
    ///
    /// If the build completed before the render delay (instant/cached builds),
    /// no output is produced to avoid flickering.
    pub fn finish(&mut self) {
        // Always ensure cursor is visible when we're done.
        let _ = crossterm::execute!(std::io::stderr(), cursor::Show);

        if !self.has_rendered {
            return;
        }
        self.clear_display();

        // If nothing was actually built (everything was cached/substituted),
        // don't print the summary table.
        if self.build_done == 0 && self.build_failed == 0 && self.download_done == 0 {
            return;
        }

        let elapsed = format_duration(self.start_time.elapsed());
        let mut stderr = std::io::stderr();

        // Show all failures.
        let failures: Vec<_> = self
            .completed
            .iter()
            .filter(|(_, s, _)| *s == DrvState::Failed)
            .collect();
        if !failures.is_empty() {
            let _ = writeln!(
                stderr,
                " {} {}",
                "Failures".red().bold(),
                "━".repeat(34).dim()
            );
            for (name, ..) in &failures {
                let _ = writeln!(stderr, "  {} {}", "✘".red().bold(), name.red());
            }
            let _ = writeln!(stderr);
        }

        // Final summary table.
        let sep = format!(" {}", "━".repeat(42).dim());
        let _ = writeln!(stderr, "{sep}");

        if self.build_done > 0 || self.build_failed > 0 {
            let _ = writeln!(
                stderr,
                "  {} {} {}  {} {}",
                "Builds   ".bold(),
                "✔".green(),
                self.build_done.to_string().green(),
                "✘".red(),
                self.build_failed.to_string().red(),
            );
        }
        if self.download_done > 0 {
            let _ = writeln!(
                stderr,
                "  {} {} {}",
                "Downloads".bold(),
                "✔".green(),
                self.download_done.to_string().green(),
            );
        }
        let _ = writeln!(stderr, "  {} {} {}", "Time     ".bold(), "⏱".dim(), elapsed,);
        let _ = writeln!(stderr, "{sep}");
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
