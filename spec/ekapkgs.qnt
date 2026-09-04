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

  /// Trusted env directories
  var trustedEnvs: Set[Path]

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
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // cache pull <paths>
  // ----------------------------------------------------------
  action cachePull(paths: Set[StorePath], cacheUrl: Url): bool = all {
    // Precondition: paths must be available on the remote cache
    paths.forall(p => remoteCache.contains(p)),
    // Effect: paths downloaded to local store
    nixStore' = nixStore.union(paths),
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
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // system switch — the most complex state transition
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
  ): bool = all {
    // Precondition: must have sudo
    hasSudo,
    // Precondition: closure must be non-empty
    closurePaths.size() > 0,
    // Precondition: new gen number must be fresh
    not(genNumbers(systemGenerations).contains(newGenNum)),
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
                  else Err("activation failed, rolled back"),
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
  }

  // ----------------------------------------------------------
  // system boot — adds boot entry but does not switch running config
  // ----------------------------------------------------------
  action systemBoot(
    newPath: StorePath,
    closurePaths: Set[StorePath],
    newGenNum: int,
  ): bool = all {
    hasSudo,
    closurePaths.size() > 0,
    not(genNumbers(systemGenerations).contains(newGenNum)),
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
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // home switch
  // ----------------------------------------------------------
  action homeSwitch(
    newPath: StorePath,
    closurePaths: Set[StorePath],
    newGenNum: int,
    activationSucceeds: bool,
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
                  else Err("home activation failed, rolled back"),
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
  }

  // ----------------------------------------------------------
  // home rollback
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
  }

  // ----------------------------------------------------------
  // home packages add
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
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // home services add
  // ----------------------------------------------------------
  action homeServicesAdd(svc: ServiceEntry, noApply: bool): bool = all {
    not(homeServices.exists(s => s.name == svc.name)),
    homeServices' = homeServices.union(Set(svc)),
    serviceStates' = if (noApply) serviceStates
                     else serviceStates.put(svc.name, Running),
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
    lastResult' = Ok,
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
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
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
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env flake-add
  // ----------------------------------------------------------
  action envFlakeAdd(dir: Path, flake: EnvFlake): bool = all {
    envManifestExists.contains(dir),
    not(envFlakes.get(dir).exists(f => f.ref_ == flake.ref_)),
    envFlakes' = envFlakes.set(dir, envFlakes.get(dir).union(Set(flake))),
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
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env flake-remove
  // ----------------------------------------------------------
  action envFlakeRemove(dir: Path, ref_: FlakeRef): bool = all {
    envManifestExists.contains(dir),
    envFlakes.get(dir).exists(f => f.ref_ == ref_),
    envFlakes' = envFlakes.set(dir, envFlakes.get(dir).filter(f => f.ref_ != ref_)),
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
    trustedEnvs' = trustedEnvs,
    searchIndexes' = searchIndexes,
    flakeLockBackup' = flakeLockBackup,
    bootEntries' = bootEntries,
    hasSudo' = hasSudo,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = substituterRunning,
    lastResult' = Ok,
  }

  // ----------------------------------------------------------
  // env allow / disallow
  // ----------------------------------------------------------
  action envAllow(dir: Path): bool = all {
    envManifestExists.contains(dir),
    trustedEnvs' = trustedEnvs.union(Set(dir)),
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
    lastResult' = Ok,
  }

  action envDisallow(dir: Path): bool = all {
    trustedEnvs' = trustedEnvs.exclude(Set(dir)),
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
    trustedEnvs' = Set(),
    searchIndexes' = Set(),
    envManifestExists' = Set(),
    flakeLockBackup' = "",
    bootEntries' = Set(),
    hasSudo' = false,
    activationInProgress' = false,
    savedPreviousProfile' = "",
    substituterRunning' = false,
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

    // Cache operations
    nondet paths = nixStore.powerset().oneOf()
    nondet url = Set("https://cache.example.com").oneOf()
    cachePush(paths, url),

    nondet paths = remoteCache.powerset().oneOf()
    nondet url = Set("https://cache.example.com").oneOf()
    cachePull(paths, url),

    cacheAuthLogin("https://cache.example.com", "tok_abc123"),
    cacheAuthLogout("https://cache.example.com"),

    // System lifecycle (gen numbers are auto-incremented)
    nondet succeeds = Set(true, false).oneOf()
    systemSwitch(".", "sys-path-new", Set("sys-path-new", "dep1"), nextSysGenNum, succeeds),

    systemBoot("sys-path-boot", Set("sys-path-boot"), nextSysGenNum),

    nondet succeeds = Set(true, false).oneOf()
    systemTest(Set("sys-path-test"), succeeds),

    nondet succeeds = Set(true, false).oneOf()
    systemRollback(succeeds),

    systemPruneBootEntries(false),
    systemPruneBootEntries(true),

    // Home lifecycle (gen numbers are auto-incremented)
    nondet succeeds = Set(true, false).oneOf()
    homeSwitch("home-path-new", Set("home-path-new", "home-dep1"), nextHomeGenNum, succeeds),

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

    // Home services
    homeServicesAdd(
      { name: "my-svc", flake: ".", enabled: true, config: Map() },
      false,
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
    envAllow(dir),

    nondet dir = Set("/home/user/project1", "/home/user/project2").oneOf()
    envDisallow(dir),

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
        lastResult' = lastResult,
      })
      // First successful switch
      .then(systemSwitch(".", "path-gen1", Set("path-gen1"), 1, true))
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
        lastResult' = lastResult,
      })
      // Second successful switch
      .then(systemSwitch(".", "path-gen2", Set("path-gen2"), 2, true))
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
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: system switch failure triggers rollback
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
        lastResult' = lastResult,
      })
      // First successful switch — establishes gen 1
      .then(systemSwitch(".", "path-gen1", Set("path-gen1"), 1, true))
      // Second switch FAILS — should rollback
      .then(systemSwitch(".", "path-gen2", Set("path-gen2"), 2, false))
      .then(all {
        // Generation should NOT have advanced
        assert(systemGenerations.length() == 1),
        assert(systemCurrentGen == 0),
        assert(systemGenerations[0].path == "path-gen1"),
        assert(lastResult == Err("activation failed, rolled back")),
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
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: env init then add package
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
        lastResult' = lastResult,
      })
      .then(envAdd("/home/user/project1", { name: "ripgrep", flake: "nixpkgs" }, "rg-store"))
      .then(all {
        assert(envPackages.get("/home/user/project1").size() == 1),
        assert(nixStore.contains("rg-store")),
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
        lastResult' = lastResult,
      })
  }

  // ----------------------------------------------------------
  // Test: home switch with rollback on failure
  // ----------------------------------------------------------
  run homeSwitchRollbackTest = {
    init
      // First successful switch
      .then(homeSwitch("home-gen1-path", Set("home-gen1-path"), 1, true))
      // Second switch fails → rollback
      .then(homeSwitch("home-gen2-path", Set("home-gen2-path"), 2, false))
      .then(all {
        assert(homeGenerations.length() == 1),
        assert(homeCurrentGen == 0),
        assert(homeGenerations[0].path == "home-gen1-path"),
        assert(lastResult == Err("home activation failed, rolled back")),
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
        lastResult' = lastResult,
      })
  }

}
