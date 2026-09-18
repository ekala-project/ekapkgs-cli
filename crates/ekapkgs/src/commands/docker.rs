use std::process::{Command, Stdio};

use color_eyre::eyre::eyre;
use ekapkgs_nix::NixCommand;
use yansi::Paint;

/// Build a Docker image from a nix closure using `dockerTools.streamLayeredImage`.
///
/// The strategy:
/// 1. Use `nix eval --apply` to detect `meta.mainProgram` for the entrypoint.
/// 2. Build an inline nix expression that calls `dockerTools.streamLayeredImage` with the
///    installable's closure.
/// 3. Run the resulting script, piping into `docker load` or writing to a file.
pub fn execute(
    installable: &str,
    name: Option<&str>,
    tag: &str,
    entrypoint: Option<&str>,
    cmd: Option<&[String]>,
    output: Option<&str>,
) -> color_eyre::Result<()> {
    // Derive the image name from the installable if not provided.
    let image_name = name
        .map(String::from)
        .unwrap_or_else(|| derive_image_name(installable));

    // Try to auto-detect entrypoint from meta.mainProgram.
    let entrypoint = match entrypoint {
        Some(ep) => Some(ep.to_owned()),
        None => {
            let spinner = ekapkgs_ui::progress::spinner("Detecting entrypoint...");
            let detected = detect_main_program(installable);
            spinner.finish_and_clear();
            if let Some(ref prog) = detected {
                tracing::info!("Auto-detected entrypoint: {prog}");
            }
            detected
        },
    };

    if entrypoint.is_none() {
        tracing::warn!(
            "No entrypoint detected — image will have no entrypoint. Use --entrypoint to set one."
        );
    }

    println!(
        "{} {}",
        "closure docker:".bold(),
        format!("building image {image_name}:{tag}...").dim()
    );

    // Build the nix expression for streamLayeredImage.
    let nix_expr = build_docker_expr(installable, &image_name, tag, entrypoint.as_deref(), cmd);

    tracing::debug!("Docker nix expression:\n{nix_expr}");

    // Build the streaming script.
    let spinner = ekapkgs_ui::progress::spinner("Building Docker image derivation...");

    let build_output: Vec<serde_json::Value> = NixCommand::new(&["build"])
        .arg("--expr")
        .arg(&nix_expr)
        .arg("--json")
        .arg("--impure")
        .json()?;

    spinner.finish_and_clear();

    let stream_script = build_output
        .first()
        .and_then(|o| o.get("outputs"))
        .and_then(|o| o.get("out"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre!("failed to get output path from nix build"))?;

    tracing::info!("Stream script: {stream_script}");

    match output {
        Some(path) => {
            // Write tarball to file.
            println!("{}", format!("Streaming image to {path}...").dim());

            let out_file = std::fs::File::create(path)?;
            let status = Command::new(stream_script)
                .stdout(out_file)
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| eyre!("failed to run stream script: {e}"))?;

            if !status.success() {
                return Err(eyre!("stream script exited with {status}"));
            }

            println!(
                "{} {} written to {path}",
                "done:".green().bold(),
                format!("{image_name}:{tag}").bold()
            );
        },
        None => {
            // Pipe into docker load.
            println!("{}", "Loading into Docker...".dim());

            let stream = Command::new(stream_script)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| eyre!("failed to run stream script: {e}"))?;

            let status = Command::new("docker")
                .arg("load")
                .stdin(
                    stream
                        .stdout
                        .ok_or_else(|| eyre!("no stdout from stream script"))?,
                )
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| {
                    eyre!("failed to run `docker load`: {e}\nhint: is Docker installed?")
                })?;

            if !status.success() {
                return Err(eyre!("docker load exited with {status}"));
            }

            println!(
                "{} loaded {}",
                "done:".green().bold(),
                format!("{image_name}:{tag}").bold()
            );
        },
    }

    Ok(())
}

/// Parse an installable like `nixpkgs#hello` or `.#packages.x86_64-linux.foo`
/// into a nix expression that evaluates to the package.
///
/// Returns `(pkg_expr, nixpkgs_expr)` where `nixpkgs_expr` resolves to a
/// nixpkgs package set for `dockerTools`.
fn installable_to_nix_expr(installable: &str) -> (String, String) {
    let (flake_ref, attr) = installable
        .split_once('#')
        .unwrap_or(("nixpkgs", installable));

    // Resolve the flake reference to a nix expression.
    let flake_expr =
        if flake_ref == "." || flake_ref.starts_with("./") || flake_ref.starts_with('/') {
            format!("builtins.getFlake \"path:{}\"", flake_ref)
        } else {
            format!("builtins.getFlake \"{flake_ref}\"")
        };

    // Build the package expression.  Installables can be either a plain attribute
    // name (resolved under legacyPackages) or a full dotted path.
    let pkg_expr = if attr.contains('.') {
        // Full path like `packages.x86_64-linux.hello` — traverse directly.
        format!("({flake_expr}).{attr}")
    } else {
        // Short name like `hello` — resolve under legacyPackages.<system>.
        format!("({flake_expr}).legacyPackages.${{builtins.currentSystem}}.{attr}")
    };

    // For dockerTools we need a nixpkgs.  If the flake IS nixpkgs, reuse it;
    // otherwise pull nixpkgs separately.
    let nixpkgs_expr = if flake_ref == "nixpkgs" {
        format!("({flake_expr}).legacyPackages.${{builtins.currentSystem}}")
    } else {
        "(builtins.getFlake \"nixpkgs\").legacyPackages.${builtins.currentSystem}".to_owned()
    };

    (pkg_expr, nixpkgs_expr)
}

/// Build the inline nix expression that creates a `streamLayeredImage`.
fn build_docker_expr(
    installable: &str,
    name: &str,
    tag: &str,
    entrypoint: Option<&str>,
    cmd: Option<&[String]>,
) -> String {
    let (pkg_expr, nixpkgs_expr) = installable_to_nix_expr(installable);

    let entrypoint_nix = match entrypoint {
        Some(ep) => {
            // If the entrypoint doesn't contain a slash, resolve it from the
            // package's bin directory.
            if ep.contains('/') {
                format!("  config.Entrypoint = [ \"{ep}\" ];\n")
            } else {
                format!("  config.Entrypoint = [ \"${{pkg}}/bin/{ep}\" ];\n")
            }
        },
        None => String::new(),
    };

    let cmd_nix = match cmd {
        Some(args) if !args.is_empty() => {
            let items: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
            format!("  config.Cmd = [ {} ];\n", items.join(" "))
        },
        _ => String::new(),
    };

    format!(
        r#"
let
  pkgs = {nixpkgs_expr};
  pkg = {pkg_expr};
in pkgs.dockerTools.streamLayeredImage {{
  name = "{name}";
  tag = "{tag}";
  contents = [ pkg ];
{entrypoint_nix}{cmd_nix}}}
"#
    )
}

/// Detect the main program from a package's `meta.mainProgram`.
fn detect_main_program(installable: &str) -> Option<String> {
    let output = NixCommand::new(&["eval", "--raw"])
        .arg(installable)
        .arg("--apply")
        .arg("p: p.meta.mainProgram or \"\"")
        .output()
        .ok()?;

    let prog = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if prog.is_empty() { None } else { Some(prog) }
}

/// Derive a Docker image name from a nix installable string.
///
/// `nixpkgs#hello` → `hello`
/// `.#packages.x86_64-linux.myapp` → `myapp`
fn derive_image_name(installable: &str) -> String {
    installable
        .rsplit_once('#')
        .map(|(_, attr)| attr)
        .unwrap_or(installable)
        .rsplit('.')
        .next()
        .unwrap_or(installable)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_name_from_nixpkgs() {
        assert_eq!(derive_image_name("nixpkgs#hello"), "hello");
    }

    #[test]
    fn derive_name_from_attr_path() {
        assert_eq!(derive_image_name(".#packages.x86_64-linux.myapp"), "myapp");
    }

    #[test]
    fn derive_name_from_plain() {
        assert_eq!(derive_image_name("hello"), "hello");
    }

    #[test]
    fn installable_to_nix_short_attr() {
        let (pkg, nixpkgs) = installable_to_nix_expr("nixpkgs#hello");
        assert!(pkg.contains("getFlake \"nixpkgs\""));
        assert!(pkg.contains("legacyPackages"));
        assert!(pkg.contains(".hello"));
        assert!(nixpkgs.contains("getFlake \"nixpkgs\""));
    }

    #[test]
    fn installable_to_nix_dotted_attr() {
        let (pkg, _) = installable_to_nix_expr("nixpkgs#packages.x86_64-linux.hello");
        assert!(pkg.contains("packages.x86_64-linux.hello"));
        assert!(!pkg.contains("legacyPackages"));
    }

    #[test]
    fn installable_to_nix_local_flake() {
        let (pkg, nixpkgs) = installable_to_nix_expr(".#myapp");
        assert!(pkg.contains("getFlake \"path:.\""));
        // Local flake uses separate nixpkgs for dockerTools
        assert!(nixpkgs.contains("getFlake \"nixpkgs\""));
    }

    #[test]
    fn docker_expr_with_entrypoint() {
        let expr = build_docker_expr("nixpkgs#nginx", "nginx", "latest", Some("nginx"), None);
        assert!(expr.contains("streamLayeredImage"));
        assert!(expr.contains("name = \"nginx\""));
        assert!(expr.contains("tag = \"latest\""));
        assert!(expr.contains("Entrypoint"));
        assert!(expr.contains("${pkg}/bin/nginx"));
        assert!(expr.contains("getFlake \"nixpkgs\""));
    }

    #[test]
    fn docker_expr_with_absolute_entrypoint() {
        let expr = build_docker_expr(
            "nixpkgs#nginx",
            "nginx",
            "latest",
            Some("/usr/bin/nginx"),
            None,
        );
        assert!(expr.contains("\"/usr/bin/nginx\""));
        assert!(!expr.contains("${pkg}"));
    }

    #[test]
    fn docker_expr_with_cmd() {
        let args = vec!["--config".to_owned(), "/etc/app.toml".to_owned()];
        let expr = build_docker_expr("nixpkgs#hello", "hello", "v1", Some("hello"), Some(&args));
        assert!(expr.contains("Cmd"));
        assert!(expr.contains("\"--config\" \"/etc/app.toml\""));
    }

    #[test]
    fn docker_expr_no_entrypoint() {
        let expr = build_docker_expr("nixpkgs#hello", "hello", "latest", None, None);
        assert!(expr.contains("streamLayeredImage"));
        assert!(!expr.contains("Entrypoint"));
        assert!(!expr.contains("Cmd"));
    }
}
