/// A parsed nix installable reference (e.g., `nixpkgs#hello` or `./path#pkg`).
#[derive(Debug, Clone)]
pub struct Installable {
    /// The raw string as passed by the user.
    pub raw: String,
}

impl Installable {
    pub fn new(raw: impl Into<String>) -> Self {
        Self { raw: raw.into() }
    }

    /// The flake reference portion (before `#`), if present.
    pub fn flake_ref(&self) -> Option<&str> {
        self.raw.split_once('#').map(|(f, _)| f)
    }

    /// The attribute path portion (after `#`), if present.
    pub fn attr_path(&self) -> Option<&str> {
        self.raw.split_once('#').map(|(_, a)| a)
    }
}

impl std::fmt::Display for Installable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl From<String> for Installable {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for Installable {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}
