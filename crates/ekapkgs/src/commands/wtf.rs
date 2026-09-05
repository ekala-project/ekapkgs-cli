use std::io::IsTerminal;
use std::process::{Command, Stdio};

use color_eyre::eyre::eyre;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use ekapkgs_nix::NixCommand;
use yansi::Paint;

/// Execute `ekapkgs wtf` — find a command you don't have, pick a package, run it.
pub fn execute(command_name: &str, args: &[String], flake: &str) -> color_eyre::Result<()> {
    println!(
        "{} {}",
        "wtf:".bold(),
        format!("looking for `{command_name}`...").dim()
    );

    let packages = locate_command(command_name)?;

    if packages.is_empty() {
        return Err(eyre!("no packages found providing `{command_name}`"));
    }

    let attr = if packages.len() == 1 {
        let pkg = &packages[0];
        println!(
            "{}",
            format!("found it in {pkg} — hold on to your mass...").dim()
        );
        pkg.clone()
    } else if !std::io::stdin().is_terminal() {
        println!(
            "{}",
            format!(
                "{} packages provide `{command_name}`, using first match: {}",
                packages.len(),
                packages[0]
            )
            .dim()
        );
        packages[0].clone()
    } else {
        println!(
            "{}",
            format!(
                "{} packages claim to have `{command_name}`. pick your fighter:",
                packages.len()
            )
            .dim()
        );

        let selection = Select::with_theme(&ColorfulTheme::default())
            .items(&packages)
            .default(0)
            .interact_opt()?;

        match selection {
            Some(i) => packages[i].clone(),
            None => return Ok(()),
        }
    };

    let installable = format!("{flake}#{attr}");
    tracing::info!("Running {installable}");

    let mut cmd = NixCommand::new(&["run"]).arg(&installable);
    if !args.is_empty() {
        cmd = cmd.arg("--");
        for arg in args {
            cmd = cmd.arg(arg);
        }
    }

    let err = cmd.exec().unwrap_err();
    Err(err.into())
}

/// Use `nix-locate` to find packages that provide `bin/<command>`.
///
/// Tries the local `nix-locate` first. If it isn't installed or its database
/// is missing, falls back to `nix run github:nix-community/nix-index-database`
/// which ships a pre-built index.
fn locate_command(command_name: &str) -> color_eyre::Result<Vec<String>> {
    let pattern = format!("bin/{command_name}");
    let locate_args = ["--minimal", "--whole-name"];

    // Try local nix-locate first.
    if let Ok(output) = Command::new("nix-locate")
        .args(locate_args)
        .arg(&pattern)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        if output.status.success() && !output.stdout.is_empty() {
            return Ok(parse_locate_output(&output.stdout));
        }
    }

    // Fall back to the nix-index-database flake.
    tracing::info!("local nix-locate unavailable, using nix-index-database flake");
    let mut nix_args: Vec<&str> = vec!["run", "github:nix-community/nix-index-database", "--"];
    for arg in &locate_args {
        nix_args.push(arg);
    }
    nix_args.push(&pattern);

    let output = Command::new("nix")
        .args(&nix_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| eyre!("couldn't run nix: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("nix-locate (via flake) failed: {stderr}"));
    }

    Ok(parse_locate_output(&output.stdout))
}

fn parse_locate_output(stdout: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            // nix-locate --minimal outputs "attr.output" (e.g. "cowsay.out").
            // Strip the output suffix to get the attribute name.
            l.rsplit_once('.')
                .map_or(l, |(attr, _output)| attr)
                .to_owned()
        })
        .collect()
}
