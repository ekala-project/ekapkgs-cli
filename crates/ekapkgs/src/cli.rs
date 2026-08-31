use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ekapkgs",
    about = "Nix CLI wrapper with negotiated binary cache"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub verbose: clap_verbosity_flag::Verbosity,
}

#[derive(Subcommand)]
pub enum Command {
    /// Build a nix package.
    Build {
        /// The installable to build (e.g., `nixpkgs#hello`).
        installable: String,

        /// Extra arguments passed through to nix.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Run a nix package.
    Run {
        /// The installable to run (e.g., `nixpkgs#hello`).
        installable: String,

        /// Extra arguments passed through to nix / the program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Enter a shell with the given packages available.
    Shell {
        /// The installable(s) to make available.
        #[arg(required = true)]
        installable: Vec<String>,

        /// Extra arguments passed through to nix (after `--`).
        #[arg(last = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Enter a development environment with cache pre-fetching.
    Develop {
        /// The flake reference for the dev shell (default: `.`).
        #[arg(default_value = ".")]
        installable: String,

        /// Extra arguments passed through to nix develop.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Manage binary caches.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },

    /// Analyze nix closures.
    Closure {
        #[command(subcommand)]
        command: ClosureCommand,
    },

    /// Deploy a NixOS configuration to a remote host.
    Deploy {
        /// The NixOS configuration installable (e.g., `.#nixosConfigurations.prod`).
        installable: String,

        /// Target host to deploy to (SSH destination).
        #[arg(long)]
        target_host: String,

        /// Host to build on (default: local machine).
        #[arg(long)]
        build_host: Option<String>,

        /// Activation mode.
        #[arg(long, value_enum, default_value = "switch")]
        mode: ActivationMode,

        /// Only show what would be done.
        #[arg(long)]
        dry_run: bool,
    },

    /// Flake introspection and management.
    Flake {
        #[command(subcommand)]
        command: FlakeCommand,
    },

    /// Manage flake registries.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },

    /// Manage the local nix store.
    Store {
        #[command(subcommand)]
        command: StoreCommand,
    },

    /// Show build log for a derivation.
    Log {
        /// The installable to show logs for.
        installable: String,
    },

    /// Show what will be built vs substituted (dry run).
    DryRun {
        /// The installable to analyze.
        installable: String,

        /// Extra arguments passed through to nix.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Manage per-user home configurations.
    Home {
        #[command(subcommand)]
        command: HomeCommand,
    },

    /// Manage the local system configuration (nixos-rebuild replacement).
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },

    /// Search packages, options, or files.
    Search {
        #[command(subcommand)]
        command: SearchCommand,
    },

    /// Manage directory-scoped package environments.
    Env {
        #[command(subcommand)]
        command: EnvCommand,
    },

    /// Check system health and configuration.
    Doctor,

    /// Run a local substituter proxy for transparent nix integration.
    ///
    /// Starts an HTTP server implementing the nix binary cache protocol that
    /// batches narinfo queries and negotiates with the upstream ekapkgs server.
    /// Configure nix to use it: `substituters = http://localhost:PORT`
    Substituter {
        /// Port to bind the local proxy on.
        #[arg(short, long, default_value = "7422")]
        port: u16,

        /// Upstream ekapkgs server URL.
        #[arg(long)]
        upstream: Option<String>,
    },

    /// Generate shell completions.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
pub enum CacheCommand {
    /// Push store paths or packages to a binary cache.
    Push {
        /// Store paths or installables to push.
        paths: Vec<String>,

        /// Cache URL to push to (overrides config).
        #[arg(long)]
        cache: Option<String>,

        /// Transfer only sources and derivation graph (for low-bandwidth links).
        /// The remote machine rebuilds from source instead of receiving built NARs.
        #[arg(long)]
        sources_only: bool,
    },

    /// Pull (pre-fetch) store paths from a binary cache.
    Pull {
        /// Store paths or installables to pull.
        paths: Vec<String>,

        /// Cache URL to pull from (overrides config).
        #[arg(long)]
        cache: Option<String>,
    },

    /// Pre-warm the cache by downloading the closure diff between two flake.lock versions.
    Warm {
        /// The installable to evaluate (e.g., `.#nixosConfigurations.prod`).
        installable: String,

        /// Git revision range for flake.lock diff (e.g., `HEAD~1..HEAD`).
        #[arg(long)]
        from_flake_lock_diff: Option<String>,

        /// Path to the old flake.lock file (alternative to --from-flake-lock-diff).
        #[arg(long)]
        old: Option<String>,

        /// Path to the new flake.lock file (default: ./flake.lock).
        #[arg(long)]
        new: Option<String>,

        /// Cache URL (overrides config).
        #[arg(long)]
        cache: Option<String>,
    },

    /// Configure cache authentication.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Set a push token for a cache.
    Login {
        /// Cache URL to authenticate with.
        cache: String,

        /// Bearer token for push access.
        #[arg(long)]
        token: String,
    },

    /// Remove stored credentials for a cache.
    Logout {
        /// Cache URL to remove credentials for.
        cache: String,
    },

    /// Show configured caches and auth status.
    Status,
}

#[derive(Subcommand)]
pub enum ClosureCommand {
    /// Show closure size breakdown.
    Size {
        /// The installable to analyze (e.g., `nixpkgs#hello`).
        installable: String,
    },

    /// Trace why a package depends on another.
    WhyDepends {
        /// The package to analyze.
        installable: String,

        /// The dependency to trace.
        dependency: String,
    },

    /// Diff two closures.
    Diff {
        /// First installable or store path.
        a: String,

        /// Second installable or store path.
        b: String,
    },

    /// Generate a Software Bill of Materials (SBOM) for a closure.
    ///
    /// Produces a CycloneDX 1.7 JSON document listing all packages in
    /// the runtime closure with dependency relationships. For ekaos
    /// system closures, enriches components with authoritative metadata
    /// (license, role, provenance) from the embedded package manifest.
    Sbom {
        /// The installable to generate an SBOM for (e.g., `nixpkgs#hello`
        /// or `.#config.system.build.toplevel`).
        installable: String,

        /// Output format.
        #[arg(long, value_enum, default_value = "cyclonedx")]
        format: SbomFormat,

        /// Include build-time dependencies (default: runtime only).
        #[arg(long)]
        buildtime: bool,

        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Diff two closures and show package changes.
    ///
    /// Builds both installables, compares their runtime closures by
    /// package name, and reports added, removed, and changed packages.
    /// Useful for reviewing what changed between system generations
    /// or flake input updates.
    SbomDiff {
        /// The old installable or store path.
        old: String,

        /// The new installable or store path.
        new: String,

        /// Output format.
        #[arg(long, value_enum, default_value = "text")]
        format: SbomDiffFormat,

        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<String>,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum SbomFormat {
    /// CycloneDX 1.7 JSON.
    Cyclonedx,
    /// CSV for quick inspection.
    Csv,
}

#[derive(Clone, clap::ValueEnum)]
pub enum SbomDiffFormat {
    /// Human-readable text summary (default).
    Text,
    /// JSON with structured change records.
    Json,
    /// CSV for quick inspection.
    Csv,
}

#[derive(Subcommand)]
pub enum StoreCommand {
    /// Garbage collect the nix store.
    Gc {
        /// Delete paths older than this duration (e.g., `30d`, `7d`).
        #[arg(long)]
        older_than: Option<String>,

        /// Only show what would be deleted.
        #[arg(long)]
        dry_run: bool,
    },

    /// Optimize the store by deduplicating via hardlinks.
    Optimize,

    /// Verify store integrity.
    Verify {
        /// Check all paths in the store.
        #[arg(long)]
        all: bool,

        /// Attempt to repair invalid paths.
        #[arg(long)]
        repair: bool,

        /// Minimum number of valid signatures required.
        #[arg(long)]
        sigs_needed: Option<u32>,
    },
}

#[derive(Subcommand)]
pub enum FlakeCommand {
    /// Pretty-print flake outputs tree.
    Show {
        /// Flake reference (default: current directory).
        #[arg(default_value = ".")]
        flake_ref: String,
    },

    /// Show flake input dependency tree.
    Metadata {
        /// Flake reference (default: current directory).
        #[arg(default_value = ".")]
        flake_ref: String,
    },

    /// Update a flake input and show closure size diff.
    UpdateDiff {
        /// The flake input to update.
        input: String,

        /// Installable to evaluate for closure comparison (default: `.`).
        #[arg(long, default_value = ".")]
        installable: String,
    },
}

#[derive(Subcommand)]
pub enum RegistryCommand {
    /// List all flake registry entries.
    List,

    /// Add or replace a flake in the user registry.
    Add {
        /// Flake reference to map from (e.g., `nixpkgs`).
        from: String,

        /// Flake reference to map to (e.g., `github:NixOS/nixpkgs`).
        to: String,

        /// Registry file to operate on (default: user registry).
        #[arg(long)]
        registry: Option<String>,
    },

    /// Remove a flake from the user registry.
    Remove {
        /// Flake reference to remove (e.g., `nixpkgs`).
        entry: String,

        /// Registry file to operate on (default: user registry).
        #[arg(long)]
        registry: Option<String>,
    },

    /// Pin a flake to its current version.
    Pin {
        /// Flake reference to pin (e.g., `nixpkgs`).
        entry: String,

        /// Registry file to operate on (default: user registry).
        #[arg(long)]
        registry: Option<String>,
    },

    /// Unpin a flake by removing its user registry entry.
    ///
    /// After unpinning, the flake reference falls through to the
    /// system or global registry, restoring the default (floating)
    /// resolution.
    Unpin {
        /// Flake reference to unpin (e.g., `nixpkgs`).
        entry: String,

        /// Registry file to operate on (default: user registry).
        #[arg(long)]
        registry: Option<String>,
    },

    /// Resolve flake references using the registry.
    Resolve {
        /// Flake references to resolve.
        refs: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum HomeCommand {
    /// Build and activate the home configuration.
    Switch {
        /// The installable for the home configuration
        /// (e.g., `.#config.system.build.home`).
        #[arg(default_value = ".#config.system.build.home")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build the home configuration without activating.
    Build {
        /// The installable for the home configuration
        /// (e.g., `.#config.system.build.home`).
        #[arg(default_value = ".#config.system.build.home")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// List home configuration generations.
    Generations,

    /// Manage imperatively-installed home packages.
    Packages {
        #[command(subcommand)]
        command: HomePackagesCommand,
    },
}

#[derive(Subcommand)]
pub enum HomePackagesCommand {
    /// Add packages to the home configuration.
    Add {
        /// Package names or attribute paths (e.g., `alacritty`,
        /// `python311Packages.requests`).
        #[arg(required = true)]
        packages: Vec<String>,

        /// Flake to resolve packages from (overrides manifest default).
        #[arg(long)]
        flake: Option<String>,
    },

    /// Remove packages from the home configuration.
    Remove {
        /// Package names to remove.
        #[arg(required = true)]
        packages: Vec<String>,
    },

    /// List imperatively-installed packages.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Export the package manifest for syncing to another machine.
    Export {
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Import a package manifest from another machine.
    Import {
        /// Path to the manifest file.
        file: String,

        /// Merge with existing packages instead of replacing.
        #[arg(long)]
        merge: bool,
    },
}

#[derive(Subcommand)]
pub enum SystemCommand {
    /// Build the system configuration and activate it.
    ///
    /// Builds the system toplevel, updates the system profile, installs
    /// boot entries, and activates the new configuration.
    Switch {
        /// The system configuration installable
        /// (e.g., `.#nixosConfigurations.myhost.config.system.build.toplevel`).
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Only show what would be done.
        #[arg(long)]
        dry_run: bool,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build and activate, but only add the boot entry without switching.
    ///
    /// The new configuration becomes the default boot entry but the
    /// running system is not changed until the next reboot.
    Boot {
        /// The system configuration installable.
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build and activate without updating the boot entry.
    ///
    /// Useful for testing a configuration without committing to it
    /// across reboots.
    Test {
        /// The system configuration installable.
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build the system configuration without activating.
    Build {
        /// The system configuration installable.
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// List system generations.
    ListGenerations,

    /// Roll back to the previous system generation.
    Rollback {
        /// Only show what would be done.
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove boot entries for generations that no longer exist.
    ///
    /// After garbage-collecting old system generations with
    /// `ekapkgs store gc`, their boot entries remain on the ESP.
    /// This command removes those orphaned entries and their
    /// associated kernel/initrd/UKI files.
    ///
    /// Use `--gc` to run garbage collection first, removing old
    /// profile generations before pruning their boot entries.
    PruneBootEntries {
        /// Boot mount point (ESP or XBOOTLDR).
        #[arg(long, default_value = "/boot")]
        boot_mount: String,

        /// Run `nix-collect-garbage -d` before pruning to remove
        /// old generation profile links first.
        #[arg(long)]
        gc: bool,

        /// Only show what would be removed.
        #[arg(long)]
        dry_run: bool,
    },

    /// Manage imperatively-installed system packages.
    Packages {
        #[command(subcommand)]
        command: SystemPackagesCommand,
    },
}

#[derive(Subcommand)]
pub enum SystemPackagesCommand {
    /// Add packages to the system.
    Add {
        /// Package names or attribute paths (e.g., `htop`,
        /// `linuxPackages.perf`).
        #[arg(required = true)]
        packages: Vec<String>,

        /// Flake to resolve packages from (overrides manifest default).
        #[arg(long)]
        flake: Option<String>,
    },

    /// Remove packages from the system.
    Remove {
        /// Package names to remove.
        #[arg(required = true)]
        packages: Vec<String>,
    },

    /// List imperatively-installed system packages.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Export the system package manifest.
    Export {
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Import a system package manifest.
    Import {
        /// Path to the manifest file.
        file: String,

        /// Merge with existing packages instead of replacing.
        #[arg(long)]
        merge: bool,
    },
}

#[derive(Subcommand)]
pub enum EnvCommand {
    /// Initialize a new environment in the current directory.
    ///
    /// Creates a `.ekapkgs-env.toml` manifest in the current directory.
    Init {
        /// Default flake for packages.
        #[arg(long, default_value = "nixpkgs")]
        flake: String,

        /// Activate the directory's `flake.nix` dev shell.
        /// The shell hook will watch `flake.nix` and `flake.lock` for
        /// changes and reload automatically.
        #[arg(long)]
        use_flake: bool,
    },

    /// Add packages to the directory environment.
    Add {
        /// Package names or attribute paths.
        #[arg(required = true)]
        packages: Vec<String>,

        /// Flake to resolve packages from (overrides manifest default).
        #[arg(long)]
        flake: Option<String>,
    },

    /// Remove packages from the directory environment.
    Remove {
        /// Package names to remove.
        #[arg(required = true)]
        packages: Vec<String>,
    },

    /// List packages in the directory environment.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Rebuild the environment profile from the current manifest and flake state.
    Reload,

    /// Allow the environment in the current directory.
    ///
    /// Marks the current manifest as trusted so the shell hook will
    /// activate it automatically.  Must be re-run after editing the
    /// manifest.
    Allow,

    /// Disallow the environment in the current directory.
    ///
    /// Removes trust so the shell hook will no longer auto-activate.
    Disallow,

    /// Print a shell hook for automatic environment activation.
    ///
    /// Add the output to your shell configuration (e.g., `.bashrc`,
    /// `.zshrc`) to automatically activate/deactivate directory
    /// environments when navigating with `cd`.
    Hook {
        /// Shell to generate the hook for.
        #[arg(value_enum)]
        shell: EnvHookShell,
    },

    /// Print the profile bin path for a directory (used by shell hooks).
    #[command(name = "_profile-bin", hide = true)]
    ProfileBin {
        /// Directory containing the environment manifest.
        dir: String,
    },

    /// Check if a directory environment is trusted (used by shell hooks).
    #[command(name = "_is-trusted", hide = true)]
    IsTrusted {
        /// Directory to check.
        dir: String,
    },

    /// Print a fingerprint of the environment files for change detection (used by shell hooks).
    #[command(name = "_fingerprint", hide = true)]
    Fingerprint {
        /// Directory to fingerprint.
        dir: String,
    },

    /// Rebuild the profile and print the bin path (used by shell hooks).
    #[command(name = "_reload", hide = true)]
    ReloadHook {
        /// Directory to reload.
        dir: String,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum EnvHookShell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Subcommand)]
pub enum SearchCommand {
    /// Search packages by name or description.
    Packages {
        /// Search query (substring or regex).
        query: String,

        /// Flake reference to search (default: `nixpkgs`).
        #[arg(long, default_value = "nixpkgs")]
        flake: String,

        /// Output results as JSON.
        #[arg(long)]
        json: bool,

        /// Maximum number of results.
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Search configuration options by name or description.
    Options {
        /// Search query (substring or regex).
        query: String,

        /// Flake reference containing the ekaos configuration.
        #[arg(long, default_value = ".")]
        flake: String,

        /// Output results as JSON.
        #[arg(long)]
        json: bool,

        /// Maximum number of results.
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Search for files across packages.
    Files {
        /// File name or path pattern to search for.
        query: String,

        /// Output results as JSON.
        #[arg(long)]
        json: bool,

        /// Maximum number of results.
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Update search indexes.
    ///
    /// Regenerates local search indexes by evaluating the flake.
    /// Use `--remote` to download pre-built indexes instead.
    Update {
        /// Flake reference to index (default: `nixpkgs`).
        #[arg(long, default_value = "nixpkgs")]
        flake: String,

        /// Download pre-built indexes from this URL instead of generating locally.
        #[arg(long)]
        remote: Option<String>,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum ActivationMode {
    Switch,
    Boot,
    Test,
}
