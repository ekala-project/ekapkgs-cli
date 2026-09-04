// Crash-semantics extension for ekapkgs
//
// Models the `flake update-diff` command as a multi-step operation
// with a crash (SIGKILL / panic) that can fire between any two steps.
// Demonstrates that the current inline-restore pattern is unsafe:
// a crash between "nix flake update" and "fs::write(backup)" leaves
// flake.lock in the updated (wrong) state.
//
// The fix — wrapping the restore in a Drop guard — is also modeled
// and shown to be crash-safe.

module ekapkgs_crash {

  // ============================================================
  // Types
  // ============================================================

  /// The phase of a flake-update-diff operation.
  /// Models the five sequential steps in cmd_update_diff (flake.rs:177-260):
  ///
  ///   Idle         → not running
  ///   BackedUp     → backup = fs::read(flake.lock)           [line 187]
  ///   OldEvaled    → old_paths = eval(closure)                [line 191]
  ///   InputUpdated → nix flake update <input>  (DISK MUTATED) [line 196]
  ///   NewEvaled    → new_paths = eval(closure)                [line 208]
  ///   Restored     → fs::write(flake.lock, backup)            [line 212]
  ///
  /// A crash can happen at any phase except Idle.
  type UpdateDiffPhase =
    | Idle
    | BackedUp
    | OldEvaled
    | InputUpdated
    | NewEvaled
    | Restored

  // ============================================================
  // State
  // ============================================================

  /// The on-disk content of flake.lock.
  /// "original" = the user's committed lock file.
  /// "updated"  = after `nix flake update` mutated it.
  var flakeLockDisk: str

  /// The in-memory backup (Vec<u8> in Rust).
  /// "" means no backup is held.
  var flakeLockMemBackup: str

  /// Current phase of the update-diff operation.
  var updateDiffPhase: UpdateDiffPhase

  /// Whether the process is alive. false = crashed/killed.
  var processAlive: bool

  /// Whether a crash has occurred in this trace.
  /// Used to track that we're examining post-crash state.
  var crashed: bool

  // ============================================================
  // Helpers
  // ============================================================

  /// A "stutter" that keeps all state unchanged (for assertion steps).
  action stutter: bool = all {
    flakeLockDisk' = flakeLockDisk,
    flakeLockMemBackup' = flakeLockMemBackup,
    updateDiffPhase' = updateDiffPhase,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  // ============================================================
  // Init
  // ============================================================

  action init: bool = all {
    flakeLockDisk' = "original",
    flakeLockMemBackup' = "",
    updateDiffPhase' = Idle,
    processAlive' = true,
    crashed' = false,
  }

  // ============================================================
  // The update-diff operation, decomposed into sub-steps
  // ============================================================

  /// Step 1: Read flake.lock into memory (backup)
  /// Rust: let backup = std::fs::read(lock_path)?;
  action updateDiff_backup: bool = all {
    processAlive,
    updateDiffPhase == Idle,
    flakeLockDisk == "original",  // must have a lock file
    flakeLockMemBackup' = flakeLockDisk,  // read disk into memory
    flakeLockDisk' = flakeLockDisk,       // disk unchanged
    updateDiffPhase' = BackedUp,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 2: Evaluate current closure (read-only, no disk mutation)
  /// Rust: let old_paths = eval::derivation_closure_paths(&inst)?;
  action updateDiff_evalOld: bool = all {
    processAlive,
    updateDiffPhase == BackedUp,
    // No disk or backup changes — just advances the phase
    flakeLockDisk' = flakeLockDisk,
    flakeLockMemBackup' = flakeLockMemBackup,
    updateDiffPhase' = OldEvaled,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 3: Run `nix flake update` — THIS MUTATES FLAKE.LOCK ON DISK
  /// Rust: NixCommand::new(&["flake", "update"]).arg(input).stream();
  action updateDiff_nixUpdate: bool = all {
    processAlive,
    updateDiffPhase == OldEvaled,
    flakeLockDisk' = "updated",           // DISK IS NOW MUTATED
    flakeLockMemBackup' = flakeLockMemBackup, // backup still in memory
    updateDiffPhase' = InputUpdated,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 4: Evaluate new closure (read-only, disk stays "updated")
  /// Rust: let new_paths = eval::derivation_closure_paths(&inst);
  action updateDiff_evalNew: bool = all {
    processAlive,
    updateDiffPhase == InputUpdated,
    flakeLockDisk' = flakeLockDisk,       // still "updated"
    flakeLockMemBackup' = flakeLockMemBackup,
    updateDiffPhase' = NewEvaled,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 5: Restore flake.lock from backup
  /// Rust: std::fs::write(lock_path, &backup)?;
  action updateDiff_restore: bool = all {
    processAlive,
    updateDiffPhase == NewEvaled,
    flakeLockDisk' = flakeLockMemBackup,  // restore from backup
    flakeLockMemBackup' = "",             // backup consumed
    updateDiffPhase' = Restored,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 6: Operation complete, return to idle
  action updateDiff_complete: bool = all {
    processAlive,
    updateDiffPhase == Restored,
    flakeLockDisk' = flakeLockDisk,
    flakeLockMemBackup' = "",
    updateDiffPhase' = Idle,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  // ============================================================
  // Crash action: can fire at any non-idle phase
  // ============================================================

  /// Process crash (SIGKILL, panic, OOM, power loss).
  /// Memory is lost. Disk state persists as-is.
  action crash: bool = all {
    processAlive,
    updateDiffPhase != Idle,  // only crash during an operation
    // Memory is wiped — backup is lost
    flakeLockMemBackup' = "",
    // Disk is untouched — whatever state it was in, it stays
    flakeLockDisk' = flakeLockDisk,
    // Process is dead
    processAlive' = false,
    updateDiffPhase' = Idle,  // operation aborted
    crashed' = true,
  }

  // ============================================================
  // Recovery: restart after a crash
  // ============================================================

  /// Process restarts after a crash. No automatic recovery.
  /// The user is back at the terminal with whatever is on disk.
  action restart: bool = all {
    not(processAlive),
    processAlive' = true,
    // Disk and all persistent state unchanged
    flakeLockDisk' = flakeLockDisk,
    flakeLockMemBackup' = "",  // memory is fresh
    updateDiffPhase' = Idle,   // no operation in progress
    crashed' = crashed,
  }

  // ============================================================
  // State machine
  // ============================================================

  action step: bool = any {
    updateDiff_backup,
    updateDiff_evalOld,
    updateDiff_nixUpdate,
    updateDiff_evalNew,
    updateDiff_restore,
    updateDiff_complete,
    crash,
    restart,
  }

  // ============================================================
  // Invariants
  // ============================================================

  /// SAFETY PROPERTY: flake.lock on disk should always be "original"
  /// when no operation is in progress (i.e., when idle and alive).
  ///
  /// This is the property we EXPECT to be violated by the current
  /// implementation when a crash occurs between InputUpdated and Restored.
  val flakeLockSafe: bool =
    (updateDiffPhase == Idle and processAlive) implies flakeLockDisk == "original"

  /// Weaker property: if we never crashed, flake.lock should be safe.
  /// This should hold — the happy path is correct.
  val flakeLockSafeNoCrash: bool =
    (updateDiffPhase == Idle and processAlive and not(crashed)) implies
      flakeLockDisk == "original"

  /// If we're idle and alive, the backup should be cleared.
  val backupClearedWhenIdle: bool =
    (updateDiffPhase == Idle) implies flakeLockMemBackup == ""

  /// During operation, if disk is mutated, backup must exist.
  val backupExistsWhenDiskMutated: bool =
    (processAlive and flakeLockDisk == "updated") implies flakeLockMemBackup != ""

  // ============================================================
  // Temporal properties
  // ============================================================

  /// The disk is always eventually restored (liveness).
  /// This WILL be violated: after crash, no mechanism restores it.
  temporal eventuallyRestored = always(
    (flakeLockDisk == "updated") implies eventually(flakeLockDisk == "original")
  )

}

// ============================================================
// Test module: demonstrate the vulnerability
// ============================================================

module ekapkgs_crash_tests {

  import ekapkgs_crash.*

  // ----------------------------------------------------------
  // Test: happy path — no crash, flake.lock is restored
  // ----------------------------------------------------------
  run happyPathTest = {
    init
      .then(updateDiff_backup)
      .then(updateDiff_evalOld)
      .then(updateDiff_nixUpdate)
      .then(all {
        // At this point disk is "updated" and backup is in memory
        assert(flakeLockDisk == "updated"),
        assert(flakeLockMemBackup == "original"),
        stutter,
      })
      .then(updateDiff_evalNew)
      .then(updateDiff_restore)
      .then(all {
        // After restore, disk is back to "original"
        assert(flakeLockDisk == "original"),
        assert(flakeLockMemBackup == ""),
        stutter,
      })
      .then(updateDiff_complete)
      .then(all {
        assert(flakeLockDisk == "original"),
        assert(updateDiffPhase == Idle),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash BEFORE nix flake update — disk is still original (safe)
  // ----------------------------------------------------------
  run crashBeforeUpdateTest = {
    init
      .then(updateDiff_backup)
      .then(updateDiff_evalOld)
      // Crash before the disk-mutating step
      .then(crash)
      .then(all {
        assert(not(processAlive)),
        assert(flakeLockDisk == "original"),  // disk never mutated — safe!
        assert(flakeLockMemBackup == ""),      // memory lost
        stutter,
      })
      .then(restart)
      .then(all {
        assert(processAlive),
        assert(flakeLockDisk == "original"),  // all good
        assert(flakeLockSafe),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash AFTER nix flake update but BEFORE restore — VULNERABILITY
  // ----------------------------------------------------------
  run crashAfterUpdateBeforeRestoreTest = {
    init
      .then(updateDiff_backup)
      .then(updateDiff_evalOld)
      .then(updateDiff_nixUpdate)
      // CRASH! Disk is "updated", backup is in memory but memory is about to die
      .then(crash)
      .then(all {
        assert(not(processAlive)),
        assert(flakeLockDisk == "updated"),    // VULNERABILITY: disk is corrupted
        assert(flakeLockMemBackup == ""),       // backup lost with process
        assert(crashed),
        stutter,
      })
      .then(restart)
      .then(all {
        assert(processAlive),
        assert(updateDiffPhase == Idle),
        assert(flakeLockDisk == "updated"),    // STILL CORRUPTED after restart
        assert(not(flakeLockSafe)),            // invariant violated!
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash during new eval (after update, before restore) — VULNERABILITY
  // ----------------------------------------------------------
  run crashDuringNewEvalTest = {
    init
      .then(updateDiff_backup)
      .then(updateDiff_evalOld)
      .then(updateDiff_nixUpdate)
      .then(updateDiff_evalNew)
      // Crash during or just after eval, before restore
      .then(crash)
      .then(all {
        assert(flakeLockDisk == "updated"),    // VULNERABILITY
        assert(flakeLockMemBackup == ""),
        stutter,
      })
      .then(restart)
      .then(all {
        assert(flakeLockDisk == "updated"),    // still bad
        assert(not(flakeLockSafe)),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash AFTER restore — disk is safe
  // ----------------------------------------------------------
  run crashAfterRestoreTest = {
    init
      .then(updateDiff_backup)
      .then(updateDiff_evalOld)
      .then(updateDiff_nixUpdate)
      .then(updateDiff_evalNew)
      .then(updateDiff_restore)
      // Crash after restore but before complete
      .then(crash)
      .then(all {
        assert(flakeLockDisk == "original"),   // safe! restore happened
        stutter,
      })
      .then(restart)
      .then(all {
        assert(flakeLockDisk == "original"),
        assert(flakeLockSafe),                 // invariant holds
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: the weak invariant (no-crash → safe) holds on happy path
  // ----------------------------------------------------------
  run noCrashSafeTest = {
    init
      .then(updateDiff_backup)
      .then(updateDiff_evalOld)
      .then(updateDiff_nixUpdate)
      .then(updateDiff_evalNew)
      .then(updateDiff_restore)
      .then(updateDiff_complete)
      .then(all {
        assert(flakeLockSafeNoCrash),
        assert(flakeLockSafe),
        assert(backupClearedWhenIdle),
        stutter,
      })
  }

}

// ============================================================
// Fixed version: Drop-guard semantics
// ============================================================

module ekapkgs_crash_fixed {

  /// Models the FIXED version of flake update-diff where the backup
  /// is written to a temporary file on disk (not just in memory),
  /// and restored via a Drop guard.
  ///
  /// This corresponds to the fix:
  ///
  ///   let backup = std::fs::read(lock_path)?;
  ///   let backup_path = lock_path.with_extension("lock.bak");
  ///   std::fs::write(&backup_path, &backup)?;  // persist backup to disk
  ///   let _guard = scopeguard::guard((), |_| {
  ///       if backup_path.exists() {
  ///           let _ = std::fs::copy(&backup_path, lock_path);
  ///           let _ = std::fs::remove_file(&backup_path);
  ///       }
  ///   });
  ///   // ... do work ...
  ///   std::fs::write(lock_path, &backup)?;
  ///   std::fs::remove_file(&backup_path)?;

  type FixedPhase =
    | Idle
    | BackedUp        // backup written to .lock.bak on disk
    | OldEvaled
    | InputUpdated    // flake.lock mutated on disk
    | NewEvaled
    | Restored        // flake.lock restored, .lock.bak still exists
    | Cleaned         // .lock.bak removed

  // State
  var flakeLockDisk: str
  var flakeLockBakDisk: str   // the .lock.bak file on disk ("" = doesn't exist)
  var phase: FixedPhase
  var processAlive: bool
  var crashed: bool

  action stutter: bool = all {
    flakeLockDisk' = flakeLockDisk,
    flakeLockBakDisk' = flakeLockBakDisk,
    phase' = phase,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  action init: bool = all {
    flakeLockDisk' = "original",
    flakeLockBakDisk' = "",
    phase' = Idle,
    processAlive' = true,
    crashed' = false,
  }

  /// Step 1: Read flake.lock and write backup to .lock.bak ON DISK
  action fixed_backup: bool = all {
    processAlive,
    phase == Idle,
    flakeLockDisk == "original",
    flakeLockBakDisk' = flakeLockDisk,  // persist backup TO DISK
    flakeLockDisk' = flakeLockDisk,
    phase' = BackedUp,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 2: Evaluate current closure
  action fixed_evalOld: bool = all {
    processAlive,
    phase == BackedUp,
    flakeLockDisk' = flakeLockDisk,
    flakeLockBakDisk' = flakeLockBakDisk,
    phase' = OldEvaled,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 3: nix flake update — mutates disk
  action fixed_nixUpdate: bool = all {
    processAlive,
    phase == OldEvaled,
    flakeLockDisk' = "updated",
    flakeLockBakDisk' = flakeLockBakDisk,  // backup still safe on disk
    phase' = InputUpdated,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 4: Evaluate new closure
  action fixed_evalNew: bool = all {
    processAlive,
    phase == InputUpdated,
    flakeLockDisk' = flakeLockDisk,
    flakeLockBakDisk' = flakeLockBakDisk,
    phase' = NewEvaled,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 5: Restore flake.lock from backup
  action fixed_restore: bool = all {
    processAlive,
    phase == NewEvaled,
    flakeLockDisk' = flakeLockBakDisk,  // restore from disk backup
    flakeLockBakDisk' = flakeLockBakDisk,  // .bak still exists until cleanup
    phase' = Restored,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 6: Clean up .lock.bak
  action fixed_cleanup: bool = all {
    processAlive,
    phase == Restored,
    flakeLockDisk' = flakeLockDisk,
    flakeLockBakDisk' = "",  // remove .lock.bak
    phase' = Cleaned,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Step 7: Complete
  action fixed_complete: bool = all {
    processAlive,
    phase == Cleaned,
    flakeLockDisk' = flakeLockDisk,
    flakeLockBakDisk' = flakeLockBakDisk,
    phase' = Idle,
    processAlive' = processAlive,
    crashed' = crashed,
  }

  /// Crash: process dies, disk persists
  action crash: bool = all {
    processAlive,
    phase != Idle,
    flakeLockDisk' = flakeLockDisk,       // disk persists
    flakeLockBakDisk' = flakeLockBakDisk, // .bak persists too!
    processAlive' = false,
    phase' = Idle,
    crashed' = true,
  }

  /// Recovery: restart and check for .bak file.
  /// If .bak exists, restore from it (this is the recovery protocol).
  action recover: bool = all {
    not(processAlive),
    if (flakeLockBakDisk != "") all {
      // Recovery: .bak exists, restore flake.lock from it
      flakeLockDisk' = flakeLockBakDisk,
      flakeLockBakDisk' = "",  // clean up .bak after recovery
    } else all {
      // No .bak → nothing to recover
      flakeLockDisk' = flakeLockDisk,
      flakeLockBakDisk' = flakeLockBakDisk,
    },
    processAlive' = true,
    phase' = Idle,
    crashed' = crashed,
  }

  action step: bool = any {
    fixed_backup,
    fixed_evalOld,
    fixed_nixUpdate,
    fixed_evalNew,
    fixed_restore,
    fixed_cleanup,
    fixed_complete,
    crash,
    recover,
  }

  // ============================================================
  // Invariants for the fixed version
  // ============================================================

  /// SAFETY: after recovery (idle + alive), flake.lock is always "original".
  /// This should NOW hold even with crashes.
  val flakeLockSafeFixed: bool =
    (phase == Idle and processAlive) implies flakeLockDisk == "original"

  /// Backup file is cleaned up when idle and alive (no .bak leak)
  val noStaleBak: bool =
    (phase == Idle and processAlive) implies flakeLockBakDisk == ""

  /// Combined safety
  val allFixedInvariants: bool = and {
    flakeLockSafeFixed,
    noStaleBak,
  }

}

// ============================================================
// Tests for the fixed version
// ============================================================

module ekapkgs_crash_fixed_tests {

  import ekapkgs_crash_fixed.*

  // ----------------------------------------------------------
  // Test: happy path (no crash)
  // ----------------------------------------------------------
  run fixedHappyPathTest = {
    init
      .then(fixed_backup)
      .then(fixed_evalOld)
      .then(fixed_nixUpdate)
      .then(all {
        assert(flakeLockDisk == "updated"),
        assert(flakeLockBakDisk == "original"),  // backup safe ON DISK
        stutter,
      })
      .then(fixed_evalNew)
      .then(fixed_restore)
      .then(all {
        assert(flakeLockDisk == "original"),
        assert(flakeLockBakDisk == "original"),  // .bak still exists
        stutter,
      })
      .then(fixed_cleanup)
      .then(all {
        assert(flakeLockBakDisk == ""),          // .bak removed
        stutter,
      })
      .then(fixed_complete)
      .then(all {
        assert(flakeLockSafeFixed),
        assert(noStaleBak),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash after nix update, recovery restores flake.lock
  // ----------------------------------------------------------
  run fixedCrashAfterUpdateTest = {
    init
      .then(fixed_backup)
      .then(fixed_evalOld)
      .then(fixed_nixUpdate)
      .then(all {
        assert(flakeLockDisk == "updated"),
        assert(flakeLockBakDisk == "original"),
        stutter,
      })
      // CRASH!
      .then(crash)
      .then(all {
        assert(not(processAlive)),
        assert(flakeLockDisk == "updated"),      // disk still bad
        assert(flakeLockBakDisk == "original"),  // but .bak survived!
        stutter,
      })
      // Recovery
      .then(recover)
      .then(all {
        assert(processAlive),
        assert(flakeLockDisk == "original"),      // RESTORED from .bak!
        assert(flakeLockBakDisk == ""),           // .bak cleaned up
        assert(flakeLockSafeFixed),               // invariant holds!
        assert(noStaleBak),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash during eval (after update), recovery restores
  // ----------------------------------------------------------
  run fixedCrashDuringNewEvalTest = {
    init
      .then(fixed_backup)
      .then(fixed_evalOld)
      .then(fixed_nixUpdate)
      .then(fixed_evalNew)
      // CRASH during eval
      .then(crash)
      .then(all {
        assert(flakeLockDisk == "updated"),
        assert(flakeLockBakDisk == "original"),
        stutter,
      })
      .then(recover)
      .then(all {
        assert(flakeLockDisk == "original"),      // fixed!
        assert(flakeLockSafeFixed),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash before disk mutation — trivially safe
  // ----------------------------------------------------------
  run fixedCrashBeforeUpdateTest = {
    init
      .then(fixed_backup)
      .then(fixed_evalOld)
      // Crash before nix flake update
      .then(crash)
      .then(all {
        assert(flakeLockDisk == "original"),      // never mutated
        assert(flakeLockBakDisk == "original"),   // .bak exists but harmless
        stutter,
      })
      .then(recover)
      .then(all {
        // Recovery sees .bak, restores (idempotent since disk was already original)
        assert(flakeLockDisk == "original"),
        assert(flakeLockBakDisk == ""),
        assert(flakeLockSafeFixed),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: crash after restore but before cleanup — .bak leak
  // ----------------------------------------------------------
  run fixedCrashAfterRestoreTest = {
    init
      .then(fixed_backup)
      .then(fixed_evalOld)
      .then(fixed_nixUpdate)
      .then(fixed_evalNew)
      .then(fixed_restore)
      .then(all {
        assert(flakeLockDisk == "original"),
        assert(flakeLockBakDisk == "original"),  // .bak still exists
        stutter,
      })
      // Crash after restore but before cleanup
      .then(crash)
      .then(all {
        assert(flakeLockDisk == "original"),      // disk is fine
        assert(flakeLockBakDisk == "original"),   // .bak leaked
        stutter,
      })
      .then(recover)
      .then(all {
        // Recovery: .bak exists, restores (idempotent) and cleans up
        assert(flakeLockDisk == "original"),
        assert(flakeLockBakDisk == ""),           // cleaned up
        assert(flakeLockSafeFixed),
        assert(noStaleBak),
        stutter,
      })
  }

  // ----------------------------------------------------------
  // Test: double crash — crash, recover, run again, crash again
  // ----------------------------------------------------------
  run fixedDoubleCrashTest = {
    init
      .then(fixed_backup)
      .then(fixed_evalOld)
      .then(fixed_nixUpdate)
      .then(crash)          // first crash
      .then(recover)        // recovers
      .then(all {
        assert(flakeLockDisk == "original"),
        assert(flakeLockSafeFixed),
        stutter,
      })
      // Run the operation again
      .then(fixed_backup)
      .then(fixed_evalOld)
      .then(fixed_nixUpdate)
      .then(fixed_evalNew)
      .then(crash)          // second crash
      .then(recover)        // recovers again
      .then(all {
        assert(flakeLockDisk == "original"),
        assert(flakeLockSafeFixed),
        assert(noStaleBak),
        stutter,
      })
  }

}
