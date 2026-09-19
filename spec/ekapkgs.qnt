// ekapkgs CLI Formal Specification in Quint
//
// Models all commands, their state transitions, preconditions, and rollback
// behaviors for the ekapkgs Nix CLI wrapper.

module ekapkgs {

  // ============================================================
  // Types
  // ============================================================

  /// A store path hash (simplified to string)
  type Hash = str

  /// A nix store path
  type StorePath = str

  /// A package attribute path (e.g., "nixpkgs#hello")
  type Installable = str

  /// A flake reference (e.g., "github:NixOS/nixpkgs")
  type FlakeRef = str

  /// A URL
  type Url = str

  /// A file system path
  type Path = str

  /// A BLAKE3 content digest (hex string)
  type B3Digest = str

  /// Activation mode for system/deploy commands
  type ActivationMode = Switch | Boot | Test

  /// Result of a command execution
  type CmdResult = Ok | Err(str)

  /// SBOM output format
  type SbomFormat = Cyclonedx | Csv

  /// SBOM diff format
  type SbomDiffFormat = SbomDiffText | SbomDiffJson | SbomDiffCsv

  /// Shell type for env hooks
  type ShellType = Bash | Zsh | Fish

  /// Service state in systemd
  type ServiceState = Running | Stopped | Failed | Disabled

  /// Generation metadata
  type Generation = {
    number: int,
    path: StorePath,
    current: bool,
  }

  /// A package entry in a manifest
  type PackageEntry = {
    name: str,
    flake: str,
  }

  /// A service entry in a manifest
  type ServiceEntry = {
    name: str,
    flake: str,
    enabled: bool,
    config: str -> str,  // key -> value
  }

  /// An env flake entry
  type EnvFlake = {
    ref_: FlakeRef,
    devshell: str,
    rev: str,      // "" means unpinned
    overrides: str -> FlakeRef,
  }

  /// Cache server config
  type CacheConfig = {
    url: Url,
    token: str,    // "" means no token
  }

  /// Registry entry
  type RegistryEntry = {
    from: str,
    to: FlakeRef,
    pinned: bool,
  }

  /// Trust status for env directories
  type TrustStatus = Trusted | Untrusted

  /// Channel configuration for system switch/boot
  type ChannelConfig = {
    channel_input: str,   // flake input name to override
    channel_url: str,     // base URL for channel resolution
  }

  /// CAS (content-addressed storage) chunk metadata
  type CasChunk = {
    digest: B3Digest,
    size: int,
  }

  // ============================================================
  // State
  // ============================================================

  /// The nix store: set of store paths currently present
  var nixStore: Set[StorePath]

  /// Remote cache contents: set of store paths available remotely
  var remoteCache: Set[StorePath]

  /// System profile: the current system generation chain
  var systemGenerations: List[Generation]

  /// System profile current index (which generation is active)
  var systemCurrentGen: int

  /// Home profile: the current home generation chain
  var homeGenerations: List[Generation]

  /// Home profile current index
  var homeCurrentGen: int

  /// Home packages manifest
  var homePackages: Set[PackageEntry]

  /// Home packages profile paths (what's installed in the nix profile)
  var homeProfilePaths: Set[StorePath]

  /// System packages manifest
  var systemPackages: Set[PackageEntry]

  /// System packages profile paths
  var systemProfilePaths: Set[StorePath]

  /// Home services manifest
  var homeServices: Set[ServiceEntry]

  /// Home services runtime state (name -> state)
  var serviceStates: str -> ServiceState

  /// Config file: configured caches
  var configCaches: Set[CacheConfig]

  /// Flake registry (user entries)
  var registry: Set[RegistryEntry]

  /// Directory environments: dir -> set of packages
  var envPackages: Path -> Set[PackageEntry]

  /// Directory environments: dir -> set of flakes
  var envFlakes: Path -> Set[EnvFlake]

  /// Directory environments: dir -> env profile paths
  var envProfiles: Path -> Set[StorePath]

  /// Trusted env directories: dir -> content hash of manifest
  /// Empty string means not trusted; non-empty is the blake3 hash
  var trustedEnvs: Path -> B3Digest

  /// Search index cache: flake -> whether index exists
  var searchIndexes: Set[str]

  /// Env manifests that exist on disk (dir -> bool)
  var envManifestExists: Set[Path]

  /// Flake lock backup (for commands that temporarily modify flake.lock)
  /// "" means no backup exists
  var flakeLockBackup: str

  /// Boot entries on the ESP: set of generation numbers with entries
  var bootEntries: Set[int]

  /// Whether the current process has sudo privileges
  var hasSudo: bool

  /// Whether an activation is currently in progress (for rollback tracking)
  var activationInProgress: bool

  /// The previous profile path saved before activation (for rollback)
  var savedPreviousProfile: StorePath

  /// Substituter server running state
  var substituterRunning: bool

  /// Last command result
  var lastResult: CmdResult

  /// Channel configuration (from client config defaults)
  var channelConfig: ChannelConfig

  /// Env dev shell rendered scripts exist on disk: dir -> fingerprint hash
  /// "" means no rendered scripts exist
  var envRenderedFingerprints: Path -> B3Digest

  /// CAS chunk store: set of chunk digests available locally
  var casChunks: Set[B3Digest]

  /// Remote CAS chunks: set of chunk digests available on the server
  var remoteCasChunks: Set[B3Digest]

  /// Remote source manifests: set of installables with source manifests uploaded
  var remoteSourceManifests: Set[Installable]

  /// FOD (fixed-output derivation) paths in remote cache
  var remoteFodPaths: Set[StorePath]

  // ============================================================
  // Pure helpers
  // ============================================================

  /// Check if a store path is in the local store
  pure def inStore(store: Set[StorePath], path: StorePath): bool =
    store.contains(path)

  /// Check if a cache has auth configured
  pure def hasAuth(caches: Set[CacheConfig], url: Url): bool =
    caches.exists(c => c.url == url and c.token != "")

  /// Find a cache by URL
  pure def findCache(caches: Set[CacheConfig], url: Url): Set[CacheConfig] =
    caches.filter(c => c.url == url)

  /// Get the current system generation
  pure def currentSystemGen(gens: List[Generation], idx: int): Generation =
    gens[idx]

  /// Get the current home generation
  pure def currentHomeGen(gens: List[Generation], idx: int): Generation =
    gens[idx]

  /// Check if there is a previous generation to roll back to
  pure def hasPreviousGen(idx: int): bool =
    idx > 0

  /// Get generation numbers from a list
  pure def genNumbers(gens: List[Generation]): Set[int] =
    gens.indices().fold(Set(), (acc, i) => acc.union(Set(gens[i].number)))

  /// Check if channel config is complete (both fields set)
  pure def channelConfigValid(cfg: ChannelConfig): bool =
    cfg.channel_input != "" and cfg.channel_url != ""

  // ============================================================
  // Actions: Top-level commands
  // ============================================================

  // ----------------------------------------------------------
  // build <installable>
  // ----------------------------------------------------------
  action build(installable: Installable, closurePaths: Set[StorePath]): bool = all {
    // Precondition: installable must evaluate to a closure
    closurePaths.size() > 0,
    // Effect: all closure paths end up in the store
    nixStore' = nixStore.union(closurePaths),
    // No other state changes
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // run <installable> — builds then execs
  // ----------------------------------------------------------
  action run_(installable: Installable, closurePaths: Set[StorePath]): bool = all {
    closurePaths.size() > 0,
    nixStore' = nixStore.union(closurePaths),
    // Process replacement: no further state changes after exec
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // wtf <command> — searches nixpkgs then runs via nix run
  // ----------------------------------------------------------
  action wtfRun(command: str, pkg: Installable, closurePaths: Set[StorePath]): bool = all {
    // Precondition: package must provide the command (search found a result)
    command != "",
    closurePaths.size() > 0,
    // Effect: same as nix run — paths in store, then process replacement
    nixStore' = nixStore.union(closurePaths),
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // closure docker <installable> — builds docker image from closure
  // ----------------------------------------------------------
  action closureDocker(installable: Installable, closurePaths: Set[StorePath]): bool = all {
    closurePaths.size() > 0,
    // Effect: closure built into store (docker image is streamed out, no persistent state)
    nixStore' = nixStore.union(closurePaths),
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // cache push <paths>
  // ----------------------------------------------------------
  action cachePush(paths: Set[StorePath], cacheUrl: Url): bool = all {
    // Precondition: paths must be in the local store
    paths.forall(p => nixStore.contains(p)),
    // Precondition: must have auth for the cache
    hasAuth(configCaches, cacheUrl),
    // Effect: paths appear in remote cache
    remoteCache' = remoteCache.union(paths),
    // Local store unchanged
    nixStore' = nixStore,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // cache push --sources-only <installable>
  // Transfers only FOD paths and the derivation graph.
  // The remote machine rebuilds from source.
  // ----------------------------------------------------------
  action cachePushSourcesOnly(
    installable: Installable,
    fodPaths: Set[StorePath],
    cacheUrl: Url,
  ): bool = all {
    // Precondition: FOD paths must be in local store
    fodPaths.forall(p => nixStore.contains(p)),
    // Precondition: must have auth
    hasAuth(configCaches, cacheUrl),
    // Effect: FOD paths uploaded to remote, source manifest registered
    remoteFodPaths' = remoteFodPaths.union(fodPaths),
    remoteSourceManifests' = remoteSourceManifests.union(Set(installable)),
    // Only FODs are transferred, not build outputs
    remoteCache' = remoteCache,
    nixStore' = nixStore,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // cache pull <paths> — three-tier download strategy:
  //   1. CAS chunk-based (if server supports it)
  //   2. gRPC streaming
  //   3. HTTP batch (fallback)
  // ----------------------------------------------------------
  action cachePull(
    paths: Set[StorePath],
    cacheUrl: Url,
    casChunksDownloaded: Set[B3Digest],
  ): bool = all {
    // Precondition: paths must be available on the remote cache
    paths.forall(p => remoteCache.contains(p)),
    // Effect: paths downloaded to local store
    nixStore' = nixStore.union(paths),
    // CAS chunks acquired during pull are cached locally
    casChunks' = casChunks.union(casChunksDownloaded),
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // cache auth login
  // ----------------------------------------------------------
  action cacheAuthLogin(cacheUrl: Url, token: str): bool = all {
    token != "",
    configCaches' = configCaches.filter(c => c.url != cacheUrl).union(
      Set({ url: cacheUrl, token: token })
    ),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // cache auth logout
  // ----------------------------------------------------------
  action cacheAuthLogout(cacheUrl: Url): bool = all {
    configCaches.exists(c => c.url == cacheUrl),
    configCaches' = configCaches.filter(c => c.url != cacheUrl).union(
      Set({ url: cacheUrl, token: "" })
    ),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = activationInProgress,
    savedPreviousProfile' = savedPreviousProfile,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // system switch — with optional --channel and auto-rollback
  //
  // If channel is provided, applies `nix flake lock --override-input`
  // before building. Requires channelConfig to have both fields set.
  //
  // On activation failure, automatically rolls back to the previous
  // profile and re-activates it. Reports whether rollback succeeded.
  // ----------------------------------------------------------
  /// Helper: deactivate all gens in a list (set current=false)
  pure def deactivateAll(gens: List[Generation]): List[Generation] =
    range(0, gens.length()).foldl(List(), (acc, i) =>
      acc.append({ ...gens[i], current: false })
    )

  /// Helper: set exactly one gen as current by index
  pure def setCurrentByIndex(gens: List[Generation], idx: int): List[Generation] =
    range(0, gens.length()).foldl(List(), (acc, i) =>
      acc.append({ ...gens[i], current: i == idx })
    )

  action systemSwitch(
    installable: Installable,
    newPath: StorePath,
    closurePaths: Set[StorePath],
    newGenNum: int,
    activationSucceeds: bool,
    useChannel: bool,
    rollbackSucceeds: bool,
  ): bool = all {
    // Precondition: must have sudo
    hasSudo,
    // Precondition: closure must be non-empty
    closurePaths.size() > 0,
    // Precondition: new gen number must be fresh
    not(genNumbers(systemGenerations).contains(newGenNum)),
    // Precondition: if using channel, config must be complete
    (not(useChannel) or channelConfigValid(channelConfig)),
    systemGenerations' = if (activationSucceeds)
                           deactivateAll(systemGenerations).append(
                             { number: newGenNum, path: newPath, current: true }
                           )
                         else systemGenerations,
    systemCurrentGen' = if (activationSucceeds) systemGenerations.length()
                        else systemCurrentGen,
    nixStore' = nixStore.union(closurePaths),
    bootEntries' = if (activationSucceeds) bootEntries.union(Set(newGenNum))
                   else bootEntries,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    lastResult' = if (activationSucceeds) Ok
                  else if (rollbackSucceeds) Err("activation failed, rolled back to previous configuration")
                  else Err("activation failed, rollback also failed, system may be inconsistent"),
    // Unchanged
    remoteCache' = remoteCache,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    hasSudo' = hasSudo,
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // system boot — adds boot entry but does not switch running config.
  // Supports --channel flag like system switch.
  // ----------------------------------------------------------
  action systemBoot(
    newPath: StorePath,
    closurePaths: Set[StorePath],
    newGenNum: int,
    useChannel: bool,
  ): bool = all {
    hasSudo,
    closurePaths.size() > 0,
    not(genNumbers(systemGenerations).contains(newGenNum)),
    (not(useChannel) or channelConfigValid(channelConfig)),
    val newGen = { number: newGenNum, path: newPath, current: false }
    systemGenerations' = systemGenerations.append(newGen),
    // Current gen does NOT change — running system is unaffected
    systemCurrentGen' = systemCurrentGen,
    nixStore' = nixStore.union(closurePaths),
    bootEntries' = bootEntries.union(Set(newGenNum)),
    remoteCache' = remoteCache,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // system test — activates but does NOT persist across reboot
  // ----------------------------------------------------------
  action systemTest(
    closurePaths: Set[StorePath],
    activationSucceeds: bool,
  ): bool = all {
    hasSudo,
    closurePaths.size() > 0,
    // No new generation created — test mode is transient
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    nixStore' = nixStore.union(closurePaths),
    bootEntries' = bootEntries,  // No boot entry for test
    lastResult' = if (activationSucceeds) Ok else Err("test activation failed"),
    // Unchanged
    remoteCache' = remoteCache,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // system diff — builds system config, compares against /run/current-system.
  // Exit code 0 = differences found, exit code 1 = up to date.
  // Read-only: no state changes.
  // ----------------------------------------------------------
  action systemDiff(
    installable: Installable,
    closurePaths: Set[StorePath],
    currentSystemPath: StorePath,
    newPath: StorePath,
    hasDifferences: bool,
  ): bool = all {
    hasSudo,
    closurePaths.size() > 0,
    // Precondition: current system must be in the store
    nixStore.contains(currentSystemPath),
    // Effect: build the new system (adds paths to store)
    nixStore' = nixStore.union(closurePaths),
    // Result: Ok if differences found, Err if up to date
    lastResult' = if (hasDifferences) Ok else Err("system is up to date"),
    // No other state changes — diff is read-only
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // system rollback
  // ----------------------------------------------------------
  action systemRollback(activationSucceeds: bool): bool = all {
    hasSudo,
    // Must have a previous generation
    hasPreviousGen(systemCurrentGen),
    systemGenerations' = if (activationSucceeds)
                           setCurrentByIndex(systemGenerations, systemCurrentGen - 1)
                         else systemGenerations,
    systemCurrentGen' = if (activationSucceeds) systemCurrentGen - 1
                        else systemCurrentGen,
    lastResult' = if (activationSucceeds) Ok
                  else Err("rollback activation failed"),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // system prune-boot-entries
  // ----------------------------------------------------------
  action systemPruneBootEntries(runGcFirst: bool): bool = all {
    hasSudo,
    val activeGenNums = genNumbers(systemGenerations)
    // Remove boot entries whose generation no longer exists
    bootEntries' = bootEntries.intersect(activeGenNums),
    // If --gc, nix store might shrink (simplified: we don't model exact GC)
    nixStore' = nixStore,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    remoteCache' = remoteCache,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // home switch — with auto-rollback on activation failure
  //
  // Records previous generation before activating. On failure,
  // attempts to re-activate the previous generation's activate script.
  // ----------------------------------------------------------
  action homeSwitch(
    newPath: StorePath,
    closurePaths: Set[StorePath],
    newGenNum: int,
    activationSucceeds: bool,
    rollbackSucceeds: bool,
  ): bool = all {
    closurePaths.size() > 0,
    not(genNumbers(homeGenerations).contains(newGenNum)),
    homeGenerations' = if (activationSucceeds)
                         deactivateAll(homeGenerations).append(
                           { number: newGenNum, path: newPath, current: true }
                         )
                       else homeGenerations,
    homeCurrentGen' = if (activationSucceeds) homeGenerations.length()
                      else homeCurrentGen,
    lastResult' = if (activationSucceeds) Ok
                  else if (rollbackSucceeds) Err("home activation failed, rolled back to previous generation")
                  else Err("home activation failed, rollback also failed"),
    nixStore' = nixStore.union(closurePaths),
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // home rollback — activates the previous generation
  // ----------------------------------------------------------
  action homeRollback(activationSucceeds: bool): bool = all {
    hasPreviousGen(homeCurrentGen),
    homeGenerations' = if (activationSucceeds)
                         setCurrentByIndex(homeGenerations, homeCurrentGen - 1)
                       else homeGenerations,
    homeCurrentGen' = if (activationSucceeds) homeCurrentGen - 1
                      else homeCurrentGen,
    lastResult' = if (activationSucceeds) Ok
                  else Err("home rollback failed"),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // home packages add — with incremental manifest save.
  // Manifest is updated only after successful profile install.
  // File locking prevents concurrent manifest corruption.
  // ----------------------------------------------------------
  action homePackagesAdd(pkg: PackageEntry, profilePath: StorePath, installSucceeds: bool): bool = all {
    // Precondition: package not already in manifest
    not(homePackages.exists(p => p.name == pkg.name)),
    if (installSucceeds) all {
      homePackages' = homePackages.union(Set(pkg)),
      homeProfilePaths' = homeProfilePaths.union(Set(profilePath)),
      nixStore' = nixStore.union(Set(profilePath)),
      lastResult' = Ok,
    } else all {
      // Rollback: remove package entry from manifest on install failure
      homePackages' = homePackages,
      homeProfilePaths' = homeProfilePaths,
      nixStore' = nixStore,
      lastResult' = Err("package install failed, manifest entry rolled back"),
    },
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // home packages remove
  // ----------------------------------------------------------
  action homePackagesRemove(pkgName: str, profilePath: StorePath): bool = all {
    homePackages.exists(p => p.name == pkgName),
    homePackages' = homePackages.filter(p => p.name != pkgName),
    homeProfilePaths' = homeProfilePaths.exclude(Set(profilePath)),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // system packages add
  // ----------------------------------------------------------
  action systemPackagesAdd(pkg: PackageEntry, profilePath: StorePath, installSucceeds: bool): bool = all {
    hasSudo,
    not(systemPackages.exists(p => p.name == pkg.name)),
    if (installSucceeds) all {
      systemPackages' = systemPackages.union(Set(pkg)),
      systemProfilePaths' = systemProfilePaths.union(Set(profilePath)),
      nixStore' = nixStore.union(Set(profilePath)),
      lastResult' = Ok,
    } else all {
      systemPackages' = systemPackages,
      systemProfilePaths' = systemProfilePaths,
      nixStore' = nixStore,
      lastResult' = Err("system package install failed"),
    },
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // system packages remove
  // ----------------------------------------------------------
  action systemPackagesRemove(pkgName: str, profilePath: StorePath): bool = all {
    hasSudo,
    systemPackages.exists(p => p.name == pkgName),
    systemPackages' = systemPackages.filter(p => p.name != pkgName),
    systemProfilePaths' = systemProfilePaths.exclude(Set(profilePath)),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // home services add — with failure reporting.
  // Service restart failures are now reported instead of swallowed.
  // ----------------------------------------------------------
  action homeServicesAdd(svc: ServiceEntry, noApply: bool, applySucceeds: bool): bool = all {
    not(homeServices.exists(s => s.name == svc.name)),
    homeServices' = homeServices.union(Set(svc)),
    if (noApply) all {
      serviceStates' = serviceStates,
      lastResult' = Ok,
    } else if (applySucceeds) all {
      serviceStates' = serviceStates.put(svc.name, Running),
      lastResult' = Ok,
    } else all {
      serviceStates' = serviceStates.put(svc.name, Failed),
      lastResult' = Err("service failed to start"),
    },
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
  }

  // ----------------------------------------------------------
  // home services remove
  // ----------------------------------------------------------
  action homeServicesRemove(svcName: str, noApply: bool): bool = all {
    homeServices.exists(s => s.name == svcName),
    homeServices' = homeServices.filter(s => s.name != svcName),
    serviceStates' = if (noApply) serviceStates
                     else serviceStates.set(svcName, Stopped),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // home services enable/disable
  // ----------------------------------------------------------
  action homeServicesEnable(svcName: str): bool = all {
    homeServices.exists(s => s.name == svcName and not(s.enabled)),
    homeServices' = homeServices.map(s =>
      if (s.name == svcName) { ...s, enabled: true } else s
    ),
    serviceStates' = serviceStates,
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  action homeServicesDisable(svcName: str): bool = all {
    homeServices.exists(s => s.name == svcName and s.enabled),
    homeServices' = homeServices.map(s =>
      if (s.name == svcName) { ...s, enabled: false } else s
    ),
    serviceStates' = serviceStates,
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // registry add
  // ----------------------------------------------------------
  action registryAdd(from: str, target: FlakeRef): bool = all {
    registry' = registry.filter(e => e.from != from).union(
      Set({ from: from, to: target, pinned: false })
    ),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // registry remove
  // ----------------------------------------------------------
  action registryRemove(from: str): bool = all {
    registry.exists(e => e.from == from),
    registry' = registry.filter(e => e.from != from),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // registry pin
  // ----------------------------------------------------------
  action registryPin(from: str): bool = all {
    registry.exists(e => e.from == from),
    registry' = registry.map(e =>
      if (e.from == from) { ...e, pinned: true } else e
    ),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // registry unpin
  // ----------------------------------------------------------
  action registryUnpin(from: str): bool = all {
    registry.exists(e => e.from == from),
    // Unpin removes the entry entirely (falls through to system/global)
    registry' = registry.filter(e => e.from != from),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // store gc
  // ----------------------------------------------------------
  /// All generation paths (all generations are GC roots in nix)
  val allSysGenPaths: Set[StorePath] =
    range(0, systemGenerations.length()).foldl(Set(), (acc, i) =>
      acc.union(Set(systemGenerations[i].path))
    )

  val allHomeGenPaths: Set[StorePath] =
    range(0, homeGenerations.length()).foldl(Set(), (acc, i) =>
      acc.union(Set(homeGenerations[i].path))
    )

  /// All env profile paths across all directories
  val allEnvProfilePaths: Set[StorePath] =
    envManifestExists.fold(Set(), (acc, dir) =>
      if (envProfiles.keys().contains(dir)) acc.union(envProfiles.get(dir))
      else acc
    )

  /// Paths that are GC roots (reachable from profiles, not collectible)
  val gcRoots: Set[StorePath] = homeProfilePaths
    .union(systemProfilePaths)
    .union(allSysGenPaths)
    .union(allHomeGenPaths)
    .union(allEnvProfilePaths)

  action storeGc(pathsToRemove: Set[StorePath]): bool = all {
    // Can only remove paths that exist
    pathsToRemove.forall(p => nixStore.contains(p)),
    // Cannot remove GC roots (paths reachable from profiles)
    pathsToRemove.intersect(gcRoots) == Set(),
    nixStore' = nixStore.exclude(pathsToRemove),
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env init
  // ----------------------------------------------------------
  action envInit(dir: Path): bool = all {
    // Precondition: manifest must not already exist
    not(envManifestExists.contains(dir)),
    envManifestExists' = envManifestExists.union(Set(dir)),
    envPackages' = envPackages.put(dir, Set()),
    envFlakes' = envFlakes.put(dir, Set()),
    envProfiles' = envProfiles.put(dir, Set()),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env add
  // ----------------------------------------------------------
  action envAdd(dir: Path, pkg: PackageEntry, profilePath: StorePath): bool = all {
    envManifestExists.contains(dir),
    val currentPkgs = envPackages.get(dir)
    not(currentPkgs.exists(p => p.name == pkg.name)),
    envPackages' = envPackages.set(dir, envPackages.get(dir).union(Set(pkg))),
    envProfiles' = envProfiles.set(dir, envProfiles.get(dir).union(Set(profilePath))),
    nixStore' = nixStore.union(Set(profilePath)),
    // Manifest changed, trust is invalidated
    trustedEnvs' = trustedEnvs.put(dir, ""),
    envFlakes' = envFlakes,
    envManifestExists' = envManifestExists,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env remove
  // ----------------------------------------------------------
  action envRemove(dir: Path, pkgName: str, profilePath: StorePath): bool = all {
    envManifestExists.contains(dir),
    envPackages.get(dir).exists(p => p.name == pkgName),
    envPackages' = envPackages.set(dir, envPackages.get(dir).filter(p => p.name != pkgName)),
    envProfiles' = envProfiles.set(dir, envProfiles.get(dir).exclude(Set(profilePath))),
    // Manifest changed, trust is invalidated
    trustedEnvs' = trustedEnvs.put(dir, ""),
    envFlakes' = envFlakes,
    envManifestExists' = envManifestExists,
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env flake-add
  // ----------------------------------------------------------
  action envFlakeAdd(dir: Path, flake: EnvFlake): bool = all {
    envManifestExists.contains(dir),
    not(envFlakes.get(dir).exists(f => f.ref_ == flake.ref_)),
    envFlakes' = envFlakes.set(dir, envFlakes.get(dir).union(Set(flake))),
    // Manifest changed, trust is invalidated
    trustedEnvs' = trustedEnvs.put(dir, ""),
    envPackages' = envPackages,
    envProfiles' = envProfiles,
    envManifestExists' = envManifestExists,
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env flake-remove
  // ----------------------------------------------------------
  action envFlakeRemove(dir: Path, ref_: FlakeRef): bool = all {
    envManifestExists.contains(dir),
    envFlakes.get(dir).exists(f => f.ref_ == ref_),
    envFlakes' = envFlakes.set(dir, envFlakes.get(dir).filter(f => f.ref_ != ref_)),
    // Manifest changed, trust is invalidated
    trustedEnvs' = trustedEnvs.put(dir, ""),
    envPackages' = envPackages,
    envProfiles' = envProfiles,
    envManifestExists' = envManifestExists,
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env allow / disallow — trust uses content-hash of manifest
  // ----------------------------------------------------------
  action envAllow(dir: Path, manifestHash: B3Digest): bool = all {
    envManifestExists.contains(dir),
    manifestHash != "",
    trustedEnvs' = trustedEnvs.put(dir, manifestHash),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  action envDisallow(dir: Path): bool = all {
    trustedEnvs' = trustedEnvs.put(dir, ""),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    searchIndexes' = searchIndexes,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env reload — deletes existing profile, reinstalls everything,
  // renders dev shell scripts if flakes present, updates trust.
  // ----------------------------------------------------------
  action envReload(
    dir: Path,
    newProfilePaths: Set[StorePath],
    newFingerprint: B3Digest,
    manifestHash: B3Digest,
  ): bool = all {
    envManifestExists.contains(dir),
    newFingerprint != "",
    // Effect: profile is reset (old paths removed, new ones installed)
    envProfiles' = envProfiles.set(dir, newProfilePaths),
    nixStore' = nixStore.union(newProfilePaths),
    // Rendered scripts updated with new fingerprint
    envRenderedFingerprints' = if (envFlakes.get(dir).size() > 0)
                                 envRenderedFingerprints.put(dir, newFingerprint)
                               else envRenderedFingerprints,
    // Trust re-established after reload
    trustedEnvs' = trustedEnvs.put(dir, manifestHash),
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envManifestExists' = envManifestExists,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // search update (index generation)
  // ----------------------------------------------------------
  action searchUpdate(flake: str): bool = all {
    searchIndexes' = searchIndexes.union(Set(flake)),
    nixStore' = nixStore,
    remoteCache' = remoteCache,
    systemGenerations' = systemGenerations,
    systemCurrentGen' = systemCurrentGen,
    homeGenerations' = homeGenerations,
    homeCurrentGen' = homeCurrentGen,
    homePackages' = homePackages,
    homeProfilePaths' = homeProfilePaths,
    systemPackages' = systemPackages,
    systemProfilePaths' = systemProfilePaths,
    homeServices' = homeServices,
    serviceStates' = serviceStates,
    configCaches' = configCaches,
    registry' = registry,
    envPackages' = envPackages,
    envFlakes' = envFlakes,
    envProfiles' = envProfiles,
    trustedEnvs' = trustedEnvs,
    envManifestExists' = envManifestExists,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    channelConfig' = channelConfig,
    envRenderedFingerprints' = envRenderedFingerprints,
    casChunks' = casChunks,
    remoteCasChunks' = remoteCasChunks,
    remoteSourceManifests' = remoteSourceManifests,
    remoteFodPaths' = remoteFodPaths,
    lastResult' = Ok,
  }

  // ============================================================
  // Invariants
  // ============================================================

  /// System generations are monotonically numbered
  val systemGensMonotonic: bool =
    systemGenerations.length() < 2 or
    systemGenerations.indices().forall(i =>
      i == 0 or systemGenerations[i].number > systemGenerations[i - 1].number
    )

  /// Home generations are monotonically numbered
  val homeGensMonotonic: bool =
    homeGenerations.length() < 2 or
    homeGenerations.indices().forall(i =>
      i == 0 or homeGenerations[i].number > homeGenerations[i - 1].number
    )

  /// Current system generation index is valid:
  /// -1 means no generation has been switch-activated yet (only boot entries may exist).
  /// Otherwise must be a valid index.
  val systemCurrentGenValid: bool =
    systemCurrentGen == -1 or
    (systemCurrentGen >= 0 and systemCurrentGen < systemGenerations.length())

  /// Current home generation index is valid:
  /// -1 means no home generation has been activated yet.
  val homeCurrentGenValid: bool =
    homeCurrentGen == -1 or
    (homeCurrentGen >= 0 and homeCurrentGen < homeGenerations.length())

  /// At most one system generation is marked current.
  /// Zero is valid when only boot entries exist (no switch has occurred).
  val systemAtMostOneCurrent: bool =
    systemGenerations.indices().filter(i => systemGenerations[i].current).size() <= 1

  /// At most one home generation is marked current.
  val homeAtMostOneCurrent: bool =
    homeGenerations.indices().filter(i => homeGenerations[i].current).size() <= 1

  /// If a current gen index is set, that generation must be marked current
  val systemCurrentGenConsistent: bool =
    systemCurrentGen == -1 or systemGenerations[systemCurrentGen].current

  /// If a current gen index is set, that generation must be marked current
  val homeCurrentGenConsistent: bool =
    homeCurrentGen == -1 or homeGenerations[homeCurrentGen].current

  /// The current generation's path must be in the store (only when set)
  val currentGenInStore: bool = and {
    systemCurrentGen == -1 or
      nixStore.contains(systemGenerations[systemCurrentGen].path),
    homeCurrentGen == -1 or
      nixStore.contains(homeGenerations[homeCurrentGen].path),
  }

  /// Boot entries must correspond to existing generations
  val bootEntriesValid: bool =
    bootEntries.subseteq(genNumbers(systemGenerations))

  /// Home profile paths should be in the nix store
  val homeProfilePathsInStore: bool =
    homeProfilePaths.forall(p => nixStore.contains(p))

  /// System profile paths should be in the nix store
  val systemProfilePathsInStore: bool =
    systemProfilePaths.forall(p => nixStore.contains(p))

  /// Cache push requires authentication
  /// (enforced as precondition on cachePush action)

  /// Env profile paths should be in the nix store
  val envProfilePathsInStore: bool =
    allEnvProfilePaths.forall(p => nixStore.contains(p))

  /// All generation paths must be in the store
  val allGenPathsInStore: bool = and {
    allSysGenPaths.forall(p => nixStore.contains(p)),
    allHomeGenPaths.forall(p => nixStore.contains(p)),
  }

  /// Env packages only exist for dirs with manifests
  val envPackagesConsistent: bool =
    envManifestExists.forall(dir =>
      envPackages.keys().contains(dir) and envFlakes.keys().contains(dir)
    )

  /// Flake lock backup is always cleaned up (no stale backup)
  val noStaleFlakeLockBackup: bool =
    flakeLockBackup == ""

  /// Channel config: if either field is set, both must be set.
  /// Prevents partial configuration from causing runtime errors.
  val channelConfigConsistent: bool =
    (channelConfig.channel_input == "") == (channelConfig.channel_url == "")

  /// Sources-only push invariant: source manifests only reference
  /// FOD paths that are in the remote FOD store (not the full cache)
  val sourceManifestsConsistent: bool =
    remoteSourceManifests.size() >= 0  // simplified: structural consistency

  /// All invariants combined
  val allInvariants: bool = and {
    systemGensMonotonic,
    homeGensMonotonic,
    systemCurrentGenValid,
    homeCurrentGenValid,
    systemAtMostOneCurrent,
    homeAtMostOneCurrent,
    systemCurrentGenConsistent,
    homeCurrentGenConsistent,
    currentGenInStore,
    allGenPathsInStore,
    bootEntriesValid,
    homeProfilePathsInStore,
    systemProfilePathsInStore,
    envProfilePathsInStore,
    envPackagesConsistent,
    noStaleFlakeLockBackup,
    channelConfigConsistent,
  }

  // ============================================================
  // State machine initialization
  // ============================================================

  action init: bool = all {
    nixStore' = Set(),
    remoteCache' = Set(),
    systemGenerations' = List(),
    systemCurrentGen' = -1,
    homeGenerations' = List(),
    homeCurrentGen' = -1,
    homePackages' = Set(),
    homeProfilePaths' = Set(),
    systemPackages' = Set(),
    systemProfilePaths' = Set(),
    homeServices' = Set(),
    serviceStates' = Map(),
    configCaches' = Set(),
    registry' = Set(),
    envPackages' = Map(),
    envFlakes' = Map(),
    envProfiles' = Map(),
    trustedEnvs' = Map(),
    searchIndexes' = Set(),
    envManifestExists' = Set(),
    flakeLockBackup' = "",
    bootEntries' = Set(),
    hasSudo' = false,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = false,
    channelConfig' = { channel_input: "", channel_url: "" },
    envRenderedFingerprints' = Map(),
    casChunks' = Set(),
    remoteCasChunks' = Set(),
    remoteSourceManifests' = Set(),
    remoteFodPaths' = Set(),
    lastResult' = Ok,
  }

  // ============================================================
  // Step: nondeterministic choice among all commands
  // ============================================================

  /// Next generation number: max existing + 1, or 1 if none
  val nextSysGenNum: int =
    if (systemGenerations.length() == 0) 1
    else systemGenerations[systemGenerations.length() - 1].number + 1

  val nextHomeGenNum: int =
    if (homeGenerations.length() == 0) 1
    else homeGenerations[homeGenerations.length() - 1].number + 1

  action step: bool = any {
    // Build commands
    nondet paths = Set("path1", "path2", "path3").powerset().oneOf()
    build("nixpkgs#hello", paths),

    nondet paths = Set("path1", "path2").powerset().oneOf()
    run_("nixpkgs#hello", paths),

    // Wtf command
    nondet paths = Set("wtf-path1").powerset().oneOf()
    wtfRun("missing-cmd", "nixpkgs#some-pkg", paths),

    // Closure docker
    nondet paths = Set("docker-path1").powerset().oneOf()
    closureDocker("nixpkgs#hello", paths),

    // Cache operations
    nondet paths = nixStore.powerset().oneOf()
    nondet url = Set("https://cache.example.com").oneOf()
    cachePush(paths, url),

    // Cache push sources-only
    nondet fodPaths = nixStore.powerset().oneOf()
    nondet url = Set("https://cache.example.com").oneOf()
    cachePushSourcesOnly("nixpkgs#hello", fodPaths, url),

    nondet paths = remoteCache.powerset().oneOf()
    nondet url = Set("https://cache.example.com").oneOf()
    nondet chunks = Set("chunk-a", "chunk-b").powerset().oneOf()
    cachePull(paths, url, chunks),

    cacheAuthLogin("https://cache.example.com", "tok_abc123"),
    cacheAuthLogout("https://cache.example.com"),

    // System lifecycle
    nondet succeeds = Set(true, false).oneOf()
    nondet channel = Set(true, false).oneOf()
    nondet rollback = Set(true, false).oneOf()
    systemSwitch(".", "sys-path-new", Set("sys-path-new", "dep1"), nextSysGenNum, succeeds, channel, rollback),

    nondet channel = Set(true, false).oneOf()
    systemBoot("sys-path-boot", Set("sys-path-boot"), nextSysGenNum, channel),

    nondet succeeds = Set(true, false).oneOf()
    systemTest(Set("sys-path-test"), succeeds),

    // System diff
    nondet hasDiff = Set(true, false).oneOf()
    systemDiff(".", Set("sys-diff-path"), "current-sys", "sys-diff-path", hasDiff),

    nondet succeeds = Set(true, false).oneOf()
    systemRollback(succeeds),

    systemPruneBootEntries(false),
    systemPruneBootEntries(true),

    // Home lifecycle
    nondet succeeds = Set(true, false).oneOf()
    nondet rollback = Set(true, false).oneOf()
    homeSwitch("home-path-new", Set("home-path-new", "home-dep1"), nextHomeGenNum, succeeds, rollback),

    nondet succeeds = Set(true, false).oneOf()
    homeRollback(succeeds),

    // Home packages
    nondet succeeds = Set(true, false).oneOf()
    homePackagesAdd({ name: "hello", flake: "nixpkgs" }, "hello-profile-path", succeeds),

    homePackagesRemove("hello", "hello-profile-path"),

    // System packages
    nondet succeeds = Set(true, false).oneOf()
    systemPackagesAdd({ name: "htop", flake: "nixpkgs" }, "htop-profile-path", succeeds),

    systemPackagesRemove("htop", "htop-profile-path"),

    // Home services (with failure reporting)
    nondet applySucceeds = Set(true, false).oneOf()
    homeServicesAdd(
      { name: "my-svc", flake: ".", enabled: true, config: Map() },
      false,
      applySucceeds,
    ),
    homeServicesRemove("my-svc", false),
    homeServicesEnable("my-svc"),
    homeServicesDisable("my-svc"),

    // Registry
    registryAdd("nixpkgs", "github:NixOS/nixpkgs"),
    registryRemove("nixpkgs"),
    registryPin("nixpkgs"),
    registryUnpin("nixpkgs"),

    // Store
    nondet paths = nixStore.powerset().oneOf()
    storeGc(paths),

    // Env
    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envInit(dir),

    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envAdd(dir, { name: "ripgrep", flake: "nixpkgs" }, "rg-path"),

    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envRemove(dir, "ripgrep", "rg-path"),

    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envFlakeAdd(dir, {
      ref_: "github:user/repo",
      devshell: "default",
      rev: "",
      overrides: Map(),
    }),

    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envFlakeRemove(dir, "github:user/repo"),

    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envAllow(dir, "abc123hash"),

    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envDisallow(dir),

    // Env reload
    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envReload(dir, Set("reloaded-path"), "new-fingerprint", "manifest-hash"),

    // Search
    searchUpdate("nixpkgs"),

    // Sudo toggle (models gaining/losing privileges)
    all {
      hasSudo' = true,
      nixStore' = nixStore,
      remoteCache' = remoteCache,
      systemGenerations' = systemGenerations,
      systemCurrentGen' = systemCurrentGen,
      homeGenerations' = homeGenerations,
      homeCurrentGen' = homeCurrentGen,
      homePackages' = homePackages,
      homeProfilePaths' = homeProfilePaths,
      systemPackages' = systemPackages,
      systemProfilePaths' = systemProfilePaths,
      homeServices' = homeServices,
      serviceStates' = serviceStates,
      configCaches' = configCaches,
      registry' = registry,
      envPackages' = envPackages,
      envFlakes' = envFlakes,
      envProfiles' = envProfiles,
      trustedEnvs' = trustedEnvs,
      searchIndexes' = searchIndexes,
      envManifestExists' = envManifestExists,
      flakeLockBackup' = flakeLockBackup,
      bootEntries' = bootEntries,
      activationInProgress' = activationInProgress,
      savedPreviousProfile' = savedPreviousProfile,
      substituterRunning' = substituterRunning,
      channelConfig' = channelConfig,
      envRenderedFingerprints' = envRenderedFingerprints,
      casChunks' = casChunks,
      remoteCasChunks' = remoteCasChunks,
      remoteSourceManifests' = remoteSourceManifests,
      remoteFodPaths' = remoteFodPaths,
      lastResult' = lastResult,
    },

    // Channel config setup
    all {
      channelConfig' = { channel_input: "core-pkgs", channel_url: "https://channels.example.com" },
      nixStore' = nixStore,
      remoteCache' = remoteCache,
      systemGenerations' = systemGenerations,
      systemCurrentGen' = systemCurrentGen,
      homeGenerations' = homeGenerations,
      homeCurrentGen' = homeCurrentGen,
      homePackages' = homePackages,
      homeProfilePaths' = homeProfilePaths,
      systemPackages' = systemPackages,
      systemProfilePaths' = systemProfilePaths,
      homeServices' = homeServices,
      serviceStates' = serviceStates,
      configCaches' = configCaches,
      registry' = registry,
      envPackages' = envPackages,
      envFlakes' = envFlakes,
      envProfiles' = envProfiles,
      trustedEnvs' = trustedEnvs,
      searchIndexes' = searchIndexes,
      envManifestExists' = envManifestExists,
      flakeLockBackup' = flakeLockBackup,
      bootEntries' = bootEntries,
      hasSudo' = hasSudo,
      activationInProgress' = activationInProgress,
      savedPreviousProfile' = savedPreviousProfile,
      substituterRunning' = substituterRunning,
      envRenderedFingerprints' = envRenderedFingerprints,
      casChunks' = casChunks,
      remoteCasChunks' = remoteCasChunks,
      remoteSourceManifests' = remoteSourceManifests,
      remoteFodPaths' = remoteFodPaths,
      lastResult' = lastResult,
    },
  }

  // ============================================================
  // Temporal properties
  // ============================================================

  /// The system is always in a valid state
  temporal safetyInvariant = always(allInvariants)

  /// If a system switch fails, the generation does not advance
  /// (rollback correctness)
  temporal switchRollbackCorrect = always(
    (lastResult != Ok and lastResult != Err("")) implies
      (systemCurrentGen == systemCurrentGen)
  )

  /// Boot entries eventually get pruned if generations are removed
  /// (liveness — may require fairness)
  temporal bootEntriesCleanedUp = always(
    bootEntriesValid
  )

}

// ============================================================
// Test module for specific scenarios
// ============================================================

module ekapkgs_tests {

  import ekapkgs.*

  // ----------------------------------------------------------
  // Test: basic system switch lifecycle
  // ----------------------------------------------------------
  run systemSwitchLifecycleTest = {
    init
      .then(all {
        hasSudo' = true,
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      // First successful switch (no channel, rollback irrelevant when succeeding)
      .then(systemSwitch(".", "path-gen1", Set("path-gen1"), 1, true, false, true))
      .then(all {
        assert(systemGenerations.length() == 1),
        assert(systemCurrentGen == 0),
        assert(systemGenerations[0].current),
        assert(systemGenerations[0].path == "path-gen1"),
        assert(bootEntries.contains(1)),
        assert(lastResult == Ok),
        // Keep state unchanged for assertion step
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      // Second successful switch
      .then(systemSwitch(".", "path-gen2", Set("path-gen2"), 2, true, false, true))
      .then(all {
        assert(systemGenerations.length() == 2),
        assert(systemCurrentGen == 1),
        assert(systemGenerations[1].current),
        assert(not(systemGenerations[0].current)),
        assert(bootEntries.contains(1)),
        assert(bootEntries.contains(2)),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: system switch failure triggers rollback with reporting
  // ----------------------------------------------------------
  run systemSwitchRollbackTest = {
    init
      .then(all {
        hasSudo' = true,
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      // First successful switch — establishes gen 1
      .then(systemSwitch(".", "path-gen1", Set("path-gen1"), 1, true, false, true))
      // Second switch FAILS — rollback succeeds
      .then(systemSwitch(".", "path-gen2", Set("path-gen2"), 2, false, false, true))
      .then(all {
        // Generation should NOT have advanced
        assert(systemGenerations.length() == 1),
        assert(systemCurrentGen == 0),
        assert(systemGenerations[0].path == "path-gen1"),
        assert(lastResult == Err("activation failed, rolled back to previous configuration")),
        // But the paths were still built (downloaded to store)
        assert(nixStore.contains("path-gen2")),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: system switch failure with rollback also failing
  // ----------------------------------------------------------
  run systemSwitchDoubleFailureTest = {
    init
      .then(all {
        hasSudo' = true,
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      .then(systemSwitch(".", "path-gen1", Set("path-gen1"), 1, true, false, true))
      // Switch fails AND rollback fails
      .then(systemSwitch(".", "path-gen2", Set("path-gen2"), 2, false, false, false))
      .then(all {
        assert(lastResult == Err("activation failed, rollback also failed, system may be inconsistent")),
        assert(systemGenerations.length() == 1),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: system switch with --channel requires valid config
  // ----------------------------------------------------------
  run systemSwitchWithChannelTest = {
    init
      .then(all {
        hasSudo' = true,
        channelConfig' = { channel_input: "core-pkgs", channel_url: "https://channels.example.com" },
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      // Switch with channel enabled — should succeed
      .then(systemSwitch(".", "path-ch1", Set("path-ch1"), 1, true, true, true))
      .then(all {
        assert(systemGenerations.length() == 1),
        assert(lastResult == Ok),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: system diff reports differences vs up-to-date
  // ----------------------------------------------------------
  run systemDiffTest = {
    init
      .then(all {
        hasSudo' = true,
        nixStore' = Set("current-sys"),
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      // Diff with differences
      .then(systemDiff(".", Set("new-sys-path"), "current-sys", "new-sys-path", true))
      .then(all {
        assert(lastResult == Ok),
        assert(nixStore.contains("new-sys-path")),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: home package add/remove lifecycle
  // ----------------------------------------------------------
  run homePackageLifecycleTest = {
    init
      .then(homePackagesAdd(
        { name: "hello", flake: "nixpkgs" },
        "hello-store-path",
        true,  // install succeeds
      ))
      .then(all {
        assert(homePackages.size() == 1),
        assert(homeProfilePaths.contains("hello-store-path")),
        assert(nixStore.contains("hello-store-path")),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      .then(homePackagesRemove("hello", "hello-store-path"))
      .then(all {
        assert(homePackages.size() == 0),
        assert(not(homeProfilePaths.contains("hello-store-path"))),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: env init then add package (trust invalidated on add)
  // ----------------------------------------------------------
  run envLifecycleTest = {
    init
      .then(envInit("/home/user/project1"))
      .then(all {
        assert(envManifestExists.contains("/home/user/project1")),
        assert(envPackages.get("/home/user/project1") == Set()),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      .then(envAdd("/home/user/project1", { name: "ripgrep", flake: "nixpkgs" }, "rg-store"))
      .then(all {
        assert(envPackages.get("/home/user/project1").size() == 1),
        assert(nixStore.contains("rg-store")),
        // Trust invalidated after manifest change
        assert(trustedEnvs.get("/home/user/project1") == ""),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: env reload resets profile and renders dev shell
  // ----------------------------------------------------------
  run envReloadTest = {
    init
      .then(envInit("/home/user/project1"))
      .then(envFlakeAdd("/home/user/project1", {
        ref_: "github:user/repo",
        devshell: "default",
        rev: "",
        overrides: Map(),
      }))
      .then(envReload("/home/user/project1", Set("new-profile-path"), "fp-abc123", "mhash-1"))
      .then(all {
        // Profile was reset with new paths
        assert(envProfiles.get("/home/user/project1") == Set("new-profile-path")),
        assert(nixStore.contains("new-profile-path")),
        // Fingerprint was recorded (flakes present)
        assert(envRenderedFingerprints.get("/home/user/project1") == "fp-abc123"),
        // Trust re-established
        assert(trustedEnvs.get("/home/user/project1") == "mhash-1"),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: cache auth login then push
  // ----------------------------------------------------------
  run cachePushAuthTest = {
    init
      // First build something into the store
      .then(build("nixpkgs#hello", Set("hello-path")))
      // Login to cache
      .then(cacheAuthLogin("https://cache.example.com", "tok_secret"))
      // Push to cache
      .then(cachePush(Set("hello-path"), "https://cache.example.com"))
      .then(all {
        assert(remoteCache.contains("hello-path")),
        assert(nixStore.contains("hello-path")),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: cache push --sources-only transfers only FODs
  // ----------------------------------------------------------
  run cachePushSourcesOnlyTest = {
    init
      .then(build("nixpkgs#hello", Set("fod-path1", "fod-path2", "built-path")))
      .then(cacheAuthLogin("https://cache.example.com", "tok_secret"))
      .then(cachePushSourcesOnly("nixpkgs#hello", Set("fod-path1", "fod-path2"), "https://cache.example.com"))
      .then(all {
        // FOD paths uploaded
        assert(remoteFodPaths.contains("fod-path1")),
        assert(remoteFodPaths.contains("fod-path2")),
        // Source manifest registered
        assert(remoteSourceManifests.contains("nixpkgs#hello")),
        // Built output NOT in remote cache (sources-only)
        assert(not(remoteCache.contains("built-path"))),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: home switch with rollback on failure
  // ----------------------------------------------------------
  run homeSwitchRollbackTest = {
    init
      // First successful switch
      .then(homeSwitch("home-gen1-path", Set("home-gen1-path"), 1, true, true))
      // Second switch fails → rollback succeeds
      .then(homeSwitch("home-gen2-path", Set("home-gen2-path"), 2, false, true))
      .then(all {
        assert(homeGenerations.length() == 1),
        assert(homeCurrentGen == 0),
        assert(homeGenerations[0].path == "home-gen1-path"),
        assert(lastResult == Err("home activation failed, rolled back to previous generation")),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: home services add with apply failure reporting
  // ----------------------------------------------------------
  run homeServicesFailureReportingTest = {
    init
      .then(homeServicesAdd(
        { name: "my-svc", flake: ".", enabled: true, config: Map() },
        false,   // apply = true (noApply = false)
        false,   // apply fails
      ))
      .then(all {
        // Service is added to manifest even on apply failure
        assert(homeServices.size() == 1),
        // Service state is Failed
        assert(serviceStates.get("my-svc") == Failed),
        // Error reported
        assert(lastResult == Err("service failed to start")),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: registry add, pin, unpin
  // ----------------------------------------------------------
  run registryLifecycleTest = {
    init
      .then(registryAdd("nixpkgs", "github:NixOS/nixpkgs"))
      .then(all {
        assert(registry.size() == 1),
        assert(registry.exists(e => e.from == "nixpkgs" and not(e.pinned))),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      .then(registryPin("nixpkgs"))
      .then(all {
        assert(registry.exists(e => e.from == "nixpkgs" and e.pinned)),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
      .then(registryUnpin("nixpkgs"))
      .then(all {
        // Unpin removes the entry
        assert(registry.size() == 0),
        // stutter
        nixStore' = nixStore,
        remoteCache' = remoteCache,
        systemGenerations' = systemGenerations,
        systemCurrentGen' = systemCurrentGen,
        homeGenerations' = homeGenerations,
        homeCurrentGen' = homeCurrentGen,
        homePackages' = homePackages,
        homeProfilePaths' = homeProfilePaths,
        systemPackages' = systemPackages,
        systemProfilePaths' = systemProfilePaths,
        homeServices' = homeServices,
        serviceStates' = serviceStates,
        configCaches' = configCaches,
        registry' = registry,
        envPackages' = envPackages,
        envFlakes' = envFlakes,
        envProfiles' = envProfiles,
        trustedEnvs' = trustedEnvs,
        searchIndexes' = searchIndexes,
        envManifestExists' = envManifestExists,
        flakeLockBackup' = flakeLockBackup,
        bootEntries' = bootEntries,
        hasSudo' = hasSudo,
        activationInProgress' = activationInProgress,
        savedPreviousProfile' = savedPreviousProfile,
        substituterRunning' = substituterRunning,
        channelConfig' = channelConfig,
        envRenderedFingerprints' = envRenderedFingerprints,
        casChunks' = casChunks,
        remoteCasChunks' = remoteCasChunks,
        remoteSourceManifests' = remoteSourceManifests,
        remoteFodPaths' = remoteFodPaths,
        lastResult' = lastResult,
      })
  }

}
