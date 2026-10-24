/// The kind of template to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    Stdenv,
    Cmake,
    Meson,
    Rust,
    Go,
    Python,
}

impl std::fmt::Display for TemplateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdenv => write!(f, "stdenv"),
            Self::Cmake => write!(f, "cmake"),
            Self::Meson => write!(f, "meson"),
            Self::Rust => write!(f, "rust"),
            Self::Go => write!(f, "go"),
            Self::Python => write!(f, "python"),
        }
    }
}

/// How to fetch the source.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Fetcher {
    GitHub {
        owner: String,
        repo: String,
        /// Domain for self-hosted instances (e.g. `github.example.com`).
        /// `None` means `github.com`.
        domain: Option<String>,
    },
    GitLab {
        owner: String,
        repo: String,
        /// Domain for self-hosted instances (e.g. `gitlab.gnome.org`).
        /// `None` means `gitlab.com`.
        domain: Option<String>,
    },
    Local,
}

/// Placeholder hash used when the real hash is not yet known.
pub const FAKE_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// All the information needed to render a package expression.
#[derive(Debug, Clone)]
pub struct ExpressionInfo {
    pub kind: TemplateKind,
    pub pname: String,
    pub version: String,
    pub description: String,
    pub license: String,
    pub homepage: String,
    pub fetcher: Fetcher,
    pub src_hash: String,
    pub cargo_hash: String,
    pub vendor_hash: String,
    /// Whether the upstream tag has a "v" prefix (e.g. "v1.0.0" vs "1.0.0").
    pub version_has_v_prefix: bool,
}

impl ExpressionInfo {
    pub fn with_defaults(kind: TemplateKind) -> Self {
        Self {
            kind,
            pname: "CHANGEME".into(),
            version: "0.0.1".into(),
            description: "CHANGEME".into(),
            license: "unfree".into(),
            homepage: String::new(),
            fetcher: Fetcher::GitHub {
                owner: "CHANGEME".into(),
                repo: "CHANGEME".into(),
                domain: None,
            },
            src_hash: FAKE_HASH.into(),
            cargo_hash: FAKE_HASH.into(),
            vendor_hash: FAKE_HASH.into(),
            version_has_v_prefix: true,
        }
    }
}
