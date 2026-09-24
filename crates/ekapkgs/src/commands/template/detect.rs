use std::path::Path;

use super::types::TemplateKind;

/// Indicator files and the template they map to, in priority order.
const INDICATORS: &[(&str, TemplateKind)] = &[
    ("Cargo.toml", TemplateKind::Rust),
    ("go.mod", TemplateKind::Go),
    ("pyproject.toml", TemplateKind::Python),
    ("setup.py", TemplateKind::Python),
    ("setup.cfg", TemplateKind::Python),
    ("meson.build", TemplateKind::Meson),
    ("CMakeLists.txt", TemplateKind::Cmake),
    ("configure", TemplateKind::Stdenv),
    ("configure.ac", TemplateKind::Stdenv),
    ("Makefile", TemplateKind::Stdenv),
];

/// Scan a directory for build-system indicator files and return the
/// highest-priority template match.
pub fn detect_template(dir: &Path) -> Option<TemplateKind> {
    for (filename, kind) in INDICATORS {
        if dir.join(filename).exists() {
            return Some(*kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn make_dir(files: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for f in files {
            let path = dir.path().join(f);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, "").unwrap();
        }
        dir
    }

    #[test]
    fn detect_rust() {
        let dir = make_dir(&["Cargo.toml", "src/main.rs"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Rust));
    }

    #[test]
    fn detect_go() {
        let dir = make_dir(&["go.mod", "main.go"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Go));
    }

    #[test]
    fn detect_python() {
        let dir = make_dir(&["pyproject.toml"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Python));
    }

    #[test]
    fn detect_python_setup_py() {
        let dir = make_dir(&["setup.py"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Python));
    }

    #[test]
    fn detect_meson() {
        let dir = make_dir(&["meson.build"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Meson));
    }

    #[test]
    fn detect_cmake() {
        let dir = make_dir(&["CMakeLists.txt"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Cmake));
    }

    #[test]
    fn detect_stdenv_configure() {
        let dir = make_dir(&["configure"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Stdenv));
    }

    #[test]
    fn detect_stdenv_makefile() {
        let dir = make_dir(&["Makefile"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Stdenv));
    }

    #[test]
    fn detect_none() {
        let dir = make_dir(&["README.md"]);
        assert_eq!(detect_template(dir.path()), None);
    }

    #[test]
    fn rust_has_priority_over_makefile() {
        let dir = make_dir(&["Cargo.toml", "Makefile"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Rust));
    }

    #[test]
    fn meson_has_priority_over_cmake() {
        // If both meson.build and CMakeLists.txt exist, meson wins by priority order
        let dir = make_dir(&["meson.build", "CMakeLists.txt"]);
        assert_eq!(detect_template(dir.path()), Some(TemplateKind::Meson));
    }
}
