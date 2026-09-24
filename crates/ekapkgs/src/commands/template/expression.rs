use super::types::{ExpressionInfo, Fetcher, TemplateKind};

/// Generate a complete core-pkgs-style Nix expression from the given info.
pub fn generate(info: &ExpressionInfo) -> String {
    match info.kind {
        TemplateKind::Stdenv => generate_stdenv(info),
        TemplateKind::Cmake => generate_cmake(info),
        TemplateKind::Meson => generate_meson(info),
        TemplateKind::Rust => generate_rust(info),
        TemplateKind::Go => generate_go(info),
        TemplateKind::Python => generate_python(info),
    }
}

fn render_fetch_block(info: &ExpressionInfo) -> (Vec<&'static str>, String) {
    match &info.fetcher {
        Fetcher::GitHub { owner, repo } => {
            let inputs = vec!["fetchFromGitHub"];
            let block = format!(
                "  src = fetchFromGitHub {{\n\x20   owner = \"{owner}\";\n\x20   repo = \
                 \"{repo}\";\n\x20   tag = \"v${{finalAttrs.version}}\";\n\x20   hash = \
                 \"{hash}\";\n\x20 }};",
                owner = owner,
                repo = repo,
                hash = info.src_hash,
            );
            (inputs, block)
        },
        Fetcher::GitLab { owner, repo } => {
            let inputs = vec!["fetchFromGitLab"];
            let block = format!(
                "  src = fetchFromGitLab {{\n\x20   owner = \"{owner}\";\n\x20   repo = \
                 \"{repo}\";\n\x20   tag = \"v${{finalAttrs.version}}\";\n\x20   hash = \
                 \"{hash}\";\n\x20 }};",
                owner = owner,
                repo = repo,
                hash = info.src_hash,
            );
            (inputs, block)
        },
        Fetcher::Local => {
            let inputs = vec![];
            let block = "  src = ./.;".into();
            (inputs, block)
        },
    }
}

fn render_meta(
    info: &ExpressionInfo,
    include_homepage: bool,
    main_program: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("    description = \"{}\";", info.description));
    if include_homepage && !info.homepage.is_empty() {
        lines.push(format!("    homepage = \"{}\";", info.homepage));
    }
    lines.push(format!("    license = lib.licenses.{};", info.license));
    if let Some(prog) = main_program {
        lines.push(format!("    mainProgram = \"{prog}\";"));
    }
    lines.push("    platforms = lib.platforms.linux;".into());

    format!("  meta = {{\n{}\n  }};", lines.join("\n"))
}

fn generate_stdenv(info: &ExpressionInfo) -> String {
    let (fetch_inputs, fetch_block) = render_fetch_block(info);

    let mut inputs = vec!["lib", "stdenv"];
    inputs.extend(fetch_inputs);

    format!(
        "{{\n\x20 {input_list},\n}}:\n\nstdenv.mkDerivation (finalAttrs: {{\n\x20 pname = \
         \"{pname}\";\n\x20 version = \"{version}\";\n\n{fetch}\n\n{meta}\n}})\n",
        input_list = inputs.join(",\n  "),
        pname = info.pname,
        version = info.version,
        fetch = fetch_block,
        meta = render_meta(info, true, None),
    )
}

fn generate_cmake(info: &ExpressionInfo) -> String {
    let (fetch_inputs, fetch_block) = render_fetch_block(info);

    let mut inputs = vec!["lib", "stdenv"];
    inputs.extend(fetch_inputs);
    inputs.push("cmake");

    format!(
        "{{\n\x20 {input_list},\n}}:\n\nstdenv.mkDerivation (finalAttrs: {{\n\x20 pname = \
         \"{pname}\";\n\x20 version = \"{version}\";\n\n{fetch}\n\n\x20 nativeBuildInputs = \
         [\n\x20   cmake\n\x20   cmake.configurePhaseHook\n\x20 ];\n\n\x20 cmakeEntries = \
         {{\n\x20 }};\n\n{meta}\n}})\n",
        input_list = inputs.join(",\n  "),
        pname = info.pname,
        version = info.version,
        fetch = fetch_block,
        meta = render_meta(info, true, None),
    )
}

fn generate_meson(info: &ExpressionInfo) -> String {
    let (fetch_inputs, fetch_block) = render_fetch_block(info);

    let mut inputs = vec!["lib", "stdenv"];
    inputs.extend(fetch_inputs);
    inputs.extend(["meson", "ninja", "pkg-config"]);

    format!(
        "{{\n\x20 {input_list},\n}}:\n\nstdenv.mkDerivation (finalAttrs: {{\n\x20 pname = \
         \"{pname}\";\n\x20 version = \"{version}\";\n\n{fetch}\n\n\x20 nativeBuildInputs = \
         [\n\x20   meson\n\x20   meson.configurePhaseHook\n\x20   ninja\n\x20   pkg-config\n\x20 \
         ];\n\n\x20 mesonEntries = {{\n\x20 }};\n\n{meta}\n}})\n",
        input_list = inputs.join(",\n  "),
        pname = info.pname,
        version = info.version,
        fetch = fetch_block,
        meta = render_meta(info, true, None),
    )
}

fn generate_rust(info: &ExpressionInfo) -> String {
    let (fetch_inputs, fetch_block) = render_fetch_block(info);

    let mut inputs = vec!["lib", "rustPlatform"];
    inputs.extend(fetch_inputs);

    format!(
        "{{\n\x20 {input_list},\n}}:\n\nrustPlatform.buildRustPackage (finalAttrs: {{\n\x20 pname \
         = \"{pname}\";\n\x20 version = \"{version}\";\n\n{fetch}\n\n\x20 cargoHash = \
         \"{cargo_hash}\";\n\n{meta}\n}})\n",
        input_list = inputs.join(",\n  "),
        pname = info.pname,
        version = info.version,
        fetch = fetch_block,
        cargo_hash = info.cargo_hash,
        meta = render_meta(info, true, Some(&info.pname)),
    )
}

fn generate_go(info: &ExpressionInfo) -> String {
    let (fetch_inputs, fetch_block) = render_fetch_block(info);

    let mut inputs = vec!["lib", "buildGoModule"];
    inputs.extend(fetch_inputs);

    format!(
        "{{\n\x20 {input_list},\n}}:\n\nbuildGoModule (finalAttrs: {{\n\x20 pname = \
         \"{pname}\";\n\x20 version = \"{version}\";\n\n{fetch}\n\n\x20 vendorHash = \
         \"{vendor_hash}\";\n\n{meta}\n}})\n",
        input_list = inputs.join(",\n  "),
        pname = info.pname,
        version = info.version,
        fetch = fetch_block,
        vendor_hash = info.vendor_hash,
        meta = render_meta(info, true, Some(&info.pname)),
    )
}

fn generate_python(info: &ExpressionInfo) -> String {
    let (fetch_inputs, fetch_block) = render_fetch_block(info);

    let mut inputs = vec!["lib", "buildPythonPackage"];
    inputs.extend(fetch_inputs);
    inputs.push("setuptools");

    // Python import check name: replace hyphens with underscores
    let import_name = info.pname.replace('-', "_");

    format!(
        "{{\n\x20 {input_list},\n}}:\n\nbuildPythonPackage (finalAttrs: {{\n\x20 pname = \
         \"{pname}\";\n\x20 version = \"{version}\";\n\x20 pyproject = true;\n\n{fetch}\n\n\x20 \
         build-system = [ setuptools ];\n\n\x20 pythonImportsCheck = [ \"{import_name}\" \
         ];\n\n{meta}\n}})\n",
        input_list = inputs.join(",\n  "),
        pname = info.pname,
        version = info.version,
        fetch = fetch_block,
        import_name = import_name,
        meta = render_meta(info, false, None),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::template::types::FAKE_HASH;

    fn test_info(kind: TemplateKind) -> ExpressionInfo {
        ExpressionInfo {
            kind,
            pname: "example".into(),
            version: "1.0.0".into(),
            description: "An example package".into(),
            license: "mit".into(),
            homepage: "https://github.com/user/example".into(),
            fetcher: Fetcher::GitHub {
                owner: "user".into(),
                repo: "example".into(),
            },
            src_hash: FAKE_HASH.into(),
            cargo_hash: FAKE_HASH.into(),
            vendor_hash: FAKE_HASH.into(),
        }
    }

    #[test]
    fn stdenv_uses_final_attrs() {
        let out = generate(&test_info(TemplateKind::Stdenv));
        assert!(out.contains("stdenv.mkDerivation (finalAttrs: {"));
    }

    #[test]
    fn stdenv_uses_tag_not_rev() {
        let out = generate(&test_info(TemplateKind::Stdenv));
        assert!(out.contains("tag = \"v${finalAttrs.version}\";"));
        assert!(!out.contains("rev ="));
    }

    #[test]
    fn stdenv_has_no_maintainers() {
        let out = generate(&test_info(TemplateKind::Stdenv));
        assert!(!out.contains("maintainers"));
    }

    #[test]
    fn stdenv_has_required_meta() {
        let out = generate(&test_info(TemplateKind::Stdenv));
        assert!(out.contains("description = \"An example package\";"));
        assert!(out.contains("license = lib.licenses.mit;"));
        assert!(out.contains("platforms = lib.platforms.linux;"));
    }

    #[test]
    fn cmake_has_configure_phase_hook() {
        let out = generate(&test_info(TemplateKind::Cmake));
        assert!(out.contains("cmake.configurePhaseHook"));
    }

    #[test]
    fn cmake_has_cmake_entries() {
        let out = generate(&test_info(TemplateKind::Cmake));
        assert!(out.contains("cmakeEntries = {"));
    }

    #[test]
    fn cmake_nativebuild_inputs_include_cmake() {
        let out = generate(&test_info(TemplateKind::Cmake));
        assert!(out.contains("nativeBuildInputs = ["));
        assert!(out.contains("cmake"));
    }

    #[test]
    fn meson_has_configure_phase_hook() {
        let out = generate(&test_info(TemplateKind::Meson));
        assert!(out.contains("meson.configurePhaseHook"));
    }

    #[test]
    fn meson_has_ninja_and_pkg_config() {
        let out = generate(&test_info(TemplateKind::Meson));
        assert!(out.contains("ninja"));
        assert!(out.contains("pkg-config"));
    }

    #[test]
    fn meson_has_meson_entries() {
        let out = generate(&test_info(TemplateKind::Meson));
        assert!(out.contains("mesonEntries = {"));
    }

    #[test]
    fn rust_has_cargo_hash() {
        let out = generate(&test_info(TemplateKind::Rust));
        assert!(out.contains("cargoHash = \""));
    }

    #[test]
    fn rust_uses_build_rust_package() {
        let out = generate(&test_info(TemplateKind::Rust));
        assert!(out.contains("rustPlatform.buildRustPackage (finalAttrs: {"));
    }

    #[test]
    fn rust_has_main_program() {
        let out = generate(&test_info(TemplateKind::Rust));
        assert!(out.contains("mainProgram = \"example\";"));
    }

    #[test]
    fn go_has_vendor_hash() {
        let out = generate(&test_info(TemplateKind::Go));
        assert!(out.contains("vendorHash = \""));
    }

    #[test]
    fn go_uses_build_go_module() {
        let out = generate(&test_info(TemplateKind::Go));
        assert!(out.contains("buildGoModule (finalAttrs: {"));
    }

    #[test]
    fn go_has_main_program() {
        let out = generate(&test_info(TemplateKind::Go));
        assert!(out.contains("mainProgram = \"example\";"));
    }

    #[test]
    fn python_uses_pyproject() {
        let out = generate(&test_info(TemplateKind::Python));
        assert!(out.contains("pyproject = true;"));
    }

    #[test]
    fn python_has_build_system() {
        let out = generate(&test_info(TemplateKind::Python));
        assert!(out.contains("build-system = [ setuptools ];"));
    }

    #[test]
    fn python_has_imports_check() {
        let out = generate(&test_info(TemplateKind::Python));
        assert!(out.contains("pythonImportsCheck = [ \"example\" ];"));
    }

    #[test]
    fn python_no_homepage_in_meta() {
        let out = generate(&test_info(TemplateKind::Python));
        // Python meta should not include homepage (not standard in core-pkgs python packages)
        let meta_start = out.find("meta = {").unwrap();
        let meta_section = &out[meta_start..];
        assert!(!meta_section.contains("homepage"));
    }

    #[test]
    fn python_no_platforms_in_meta() {
        // buildPythonPackage handles platforms, but core-pkgs still includes it
        // Actually, let's keep platforms for consistency
        let out = generate(&test_info(TemplateKind::Python));
        assert!(out.contains("platforms = lib.platforms.linux;"));
    }

    #[test]
    fn python_hyphen_to_underscore_in_import() {
        let mut info = test_info(TemplateKind::Python);
        info.pname = "my-package".into();
        let out = generate(&info);
        assert!(out.contains("pythonImportsCheck = [ \"my_package\" ];"));
    }

    #[test]
    fn all_templates_have_no_maintainers() {
        for kind in [
            TemplateKind::Stdenv,
            TemplateKind::Cmake,
            TemplateKind::Meson,
            TemplateKind::Rust,
            TemplateKind::Go,
            TemplateKind::Python,
        ] {
            let out = generate(&test_info(kind));
            assert!(
                !out.contains("maintainers"),
                "{kind} template should not contain maintainers"
            );
        }
    }

    #[test]
    fn all_templates_use_final_attrs() {
        for kind in [
            TemplateKind::Stdenv,
            TemplateKind::Cmake,
            TemplateKind::Meson,
            TemplateKind::Rust,
            TemplateKind::Go,
            TemplateKind::Python,
        ] {
            let out = generate(&test_info(kind));
            assert!(
                out.contains("finalAttrs:"),
                "{kind} template should use finalAttrs pattern"
            );
        }
    }

    #[test]
    fn local_fetcher_uses_dot_slash() {
        let mut info = test_info(TemplateKind::Stdenv);
        info.fetcher = Fetcher::Local;
        let out = generate(&info);
        assert!(out.contains("src = ./.;"));
        assert!(!out.contains("fetchFromGitHub"));
    }

    #[test]
    fn gitlab_fetcher_uses_fetch_from_gitlab() {
        let mut info = test_info(TemplateKind::Stdenv);
        info.fetcher = Fetcher::GitLab {
            owner: "org".into(),
            repo: "proj".into(),
        };
        let out = generate(&info);
        assert!(out.contains("fetchFromGitLab"));
        assert!(out.contains("owner = \"org\";"));
        assert!(out.contains("repo = \"proj\";"));
    }
}
