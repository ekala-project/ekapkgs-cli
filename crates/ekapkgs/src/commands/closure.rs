use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, store};

use crate::cli::ClosureCommand;

pub fn execute(command: ClosureCommand) -> color_eyre::Result<()> {
    match command {
        ClosureCommand::Size { installable } => cmd_size(&installable),
        ClosureCommand::WhyDepends {
            installable,
            dependency,
        } => cmd_why_depends(&installable, &dependency),
        ClosureCommand::Diff { a, b } => cmd_diff(&a, &b),
    }
}

fn cmd_size(installable: &str) -> color_eyre::Result<()> {
    let inst = Installable::new(installable);

    let spinner = ekapkgs_ui::progress::spinner("Evaluating closure...");
    let mut entries = store::closure_path_info(&inst)?;
    spinner.finish_and_clear();

    // Sort by NAR size descending.
    entries.sort_by_key(|e| std::cmp::Reverse(e.nar_size));

    let total: u64 = entries.iter().map(|e| e.nar_size).sum();

    // Print table header.
    println!("{:>10}  Store path", "Size");
    println!("{:>10}  ----------", "----");

    for entry in &entries {
        let size = ekapkgs_ui::format::format_bytes(entry.nar_size);
        // Show just the basename of the store path.
        let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        println!("{size:>10}  {name}");
    }

    println!();
    println!(
        "{} paths, {} total",
        entries.len(),
        ekapkgs_ui::format::format_bytes(total)
    );

    Ok(())
}

fn cmd_why_depends(installable: &str, dependency: &str) -> color_eyre::Result<()> {
    NixCommand::new(&["why-depends"])
        .arg(installable)
        .arg(dependency)
        .stream()?;
    Ok(())
}

fn cmd_diff(a: &str, b: &str) -> color_eyre::Result<()> {
    NixCommand::new(&["store", "diff-closures"])
        .arg(a)
        .arg(b)
        .stream()?;
    Ok(())
}
