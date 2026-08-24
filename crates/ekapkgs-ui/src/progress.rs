use indicatif::{ProgressBar, ProgressStyle};

/// Create a spinner for indeterminate operations (evaluation, negotiation).
pub fn spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⢹", "⢺", "⢼", "⣸", "⣇", "⡧", "⡗", "⡏", " "])
            .template("{spinner:.blue} {msg}")
            .expect("valid template"),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

/// Create a progress bar for download operations.
pub fn download_bar(total_bytes: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_bytes);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({bytes_per_sec})")
            .expect("valid template")
            .progress_chars("██░"),
    );
    pb
}

/// Create a progress bar for counting items (e.g., paths).
pub fn item_bar(total: u64, unit: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(&format!(
                "  [{{bar:30.cyan/dim}}] {{pos}}/{{len}} {unit}"
            ))
            .expect("valid template")
            .progress_chars("██░"),
    );
    pb
}
