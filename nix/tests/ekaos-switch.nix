# `ekapkgs system {switch,test,boot}` on a booted ekaos VM.
#
# The VM boots generation A. Generation B is the same configuration with
# `e2e.generation = "b"`, which changes, adds and removes units and /etc
# files. B, and a copy of B that declares a contract this CLI does not
# know, are in the VM's store, so the CLI switches between them without
# building anything. Every command runs inside the guest.
{
  ekaosTest,
  ekapkgs,
}:

let
  machine =
    {
      config,
      lib,
      pkgs,
      extendModules,
      ...
    }:
    let
      gen = config.e2e.generation;
      onlyIn = g: lib.mkIf (gen == g);

      # A long-running service whose unit carries the generation
      daemon =
        extra:
        {
          wantedBy = [ "multi-user.target" ];
          environment.GEN = gen;
          script = "exec ${pkgs.coreutils}/bin/sleep infinity";
        }
        // extra;

      genB =
        (extendModules {
          modules = [ { e2e.generation = "b"; } ];
        }).config.system.build.toplevel;

      # B, but declaring a switch contract from the future
      unknownContract = pkgs.runCommand "ekaos-system-unknown-contract" { } ''
        cp -a ${genB} $out
        chmod u+w $out
        rm $out/ekaos-switch-version
        echo -n 99 > $out/ekaos-switch-version
      '';
    in
    {
      options.e2e = {
        generation = lib.mkOption {
          type = lib.types.enum [
            "a"
            "b"
          ];
          default = "a";
        };
        paths = lib.mkOption {
          type = lib.types.attrsOf lib.types.package;
          internal = true;
        };
      };

      config = {
        virtualisation.enable = true;
        boot.loader.systemd-boot.enable = true;
        environment.systemPackages = [
          ekapkgs
          pkgs.util-linux
        ];

        # A is the booted system; B and the unknown contract are built on
        # the host and copied into the image, as the guest cannot build.
        e2e.paths = { inherit genB unknownContract; };
        virtualisation.additionalPaths = lib.mkIf (gen == "a") [
          genB
          unknownContract
        ];

        systemd.services = {
          # Changed: restarted
          e2e-restart = daemon { };
          # Changed, but reloadIfChanged: reloaded in place
          e2e-reload = daemon {
            reloadIfChanged = true;
            serviceConfig.ExecReload = "${pkgs.coreutils}/bin/touch /run/e2e-reloaded";
          };
          # Changed, but restartIfChanged = false: left running
          e2e-norestart = daemon { restartIfChanged = false; };
          # Unchanged: left alone
          e2e-same = {
            wantedBy = [ "multi-user.target" ];
            script = "exec ${pkgs.coreutils}/bin/sleep infinity";
          };
          # Only in A: stopped and removed
          e2e-removed = lib.mkIf (gen == "a") (daemon { });
          # Only in B: started
          e2e-added = lib.mkIf (gen == "b") (daemon { });
        };

        environment.etc = {
          "e2e-generation".text = gen;
          "e2e-only-a" = onlyIn "a" { text = "a"; };
        };
      };
    };
in
ekaosTest {
  name = "ekaos-switch";
  meta.description = "Switch a running ekaos system with the ekapkgs CLI";

  nodes = { inherit machine; };

  testScript =
    nodes:
    let
      genA = nodes.machine.config.system.build.toplevel;
      inherit (nodes.machine.config.e2e.paths) genB unknownContract;
    in
    ''
      import re

      A = "${genA}"
      B = "${genB}"
      UNKNOWN = "${unknownContract}"
      PROFILE = "/nix/var/nix/profiles/system"
      UNITS = ["e2e-restart", "e2e-reload", "e2e-norestart", "e2e-same", "e2e-removed"]


      def pid(unit):
          return machine.succeed(f"systemctl show -p MainPID --value {unit}.service").strip()


      def pids(units):
          return {u: pid(u) for u in units}


      def current():
          return machine.succeed("readlink -f /run/current-system").strip()


      def profile():
          return machine.succeed(f"readlink -f {PROFILE}").strip()


      def ekapkgs(args):
          out = machine.succeed(f"ekapkgs system {args} 2>&1")
          print(out)
          return out


      def plan(out):
          """The unit lists of a dry-run report, keyed by what would happen."""
          lists = {}
          for line in out.splitlines():
              m = re.match(r"would (.+?) the following (?:changed )?units: (.*)", line)
              if m:
                  lists[m.group(1)] = set(m.group(2).split(", "))
          return lists


      def boot_default():
          """The toplevel of the default boot entry."""
          entry = machine.succeed("sed -n 's/^default //p' /boot/loader/loader.conf").strip()
          return machine.succeed(
              f"sed -n 's|^options init=\\(/nix/store/[^/ ]*\\)/init.*|\\1|p' /boot/loader/entries/{entry}"
          ).strip()


      machine.start()
      machine.wait_for_unit("multi-user.target")
      for u in UNITS:
          machine.wait_for_unit(f"{u}.service")
      assert current() == A, f"booted {current()}, expected {A}"
      assert profile() == A, f"profile {profile()}, expected {A}"

      with subtest("switching needs root"):
          out = machine.fail(f"runuser -u nobody -- ekapkgs system switch {B} 2>&1")
          assert "needs root" in out, out

      with subtest("an unknown contract is refused before anything changes"):
          before = pids(UNITS)
          out = machine.fail(f"ekapkgs system switch {UNKNOWN} 2>&1")
          print(out)
          assert "unsupported ekaos switch contract" in out, out
          assert current() == A and profile() == A
          assert pids(UNITS) == before

      with subtest("dry-run reports the plan and changes nothing"):
          before = pids(UNITS)
          lists = plan(ekapkgs(f"switch --dry-run {B}"))
          print(lists)
          # stopIfChanged (the default) restarts as a stop, then a start
          assert lists.get("stop") == {"e2e-removed.service", "e2e-restart.service"}, lists
          # New units are started through their targets, which the plan
          # does not list
          assert lists.get("start") == {"e2e-restart.service"}, lists
          assert lists.get("reload") == {"e2e-reload.service"}, lists
          assert lists.get("NOT stop") == {"e2e-norestart.service"}, lists
          assert "restart" not in lists, lists
          assert current() == A and profile() == A
          assert pids(UNITS) == before
          machine.succeed("test ! -e /run/e2e-reloaded")

      with subtest("test activates B without touching the profile"):
          before = pids(UNITS)
          ekapkgs(f"test {B}")
          assert current() == B, current()
          assert profile() == A, f"test moved the profile to {profile()}"

          assert pid("e2e-restart") != before["e2e-restart"], "e2e-restart was not restarted"
          machine.succeed("systemctl show -p Environment --value e2e-restart.service | grep -q GEN=b")
          assert pid("e2e-reload") == before["e2e-reload"], "e2e-reload was restarted"
          machine.succeed("test -e /run/e2e-reloaded")
          assert pid("e2e-norestart") == before["e2e-norestart"], "e2e-norestart was restarted"
          assert pid("e2e-same") == before["e2e-same"], "e2e-same was restarted"
          machine.fail("systemctl is-active e2e-removed.service")
          machine.fail("test -e /etc/systemd/system/e2e-removed.service")
          machine.succeed("systemctl is-active e2e-added.service")

          assert machine.succeed("cat /etc/e2e-generation").strip() == "b"
          machine.fail("test -e /etc/e2e-only-a")

      with subtest("switching back to A reverses it"):
          ekapkgs(f"switch {A}")
          assert current() == A and profile() == A
          machine.succeed("systemctl is-active e2e-removed.service")
          machine.fail("systemctl is-active e2e-added.service")
          assert machine.succeed("cat /etc/e2e-generation").strip() == "a"
          machine.succeed("test -e /etc/e2e-only-a")

      with subtest("boot makes B the default without activating it"):
          before = pids(UNITS)
          assert boot_default() == A, boot_default()
          ekapkgs(f"boot {B}")
          assert current() == A, f"boot activated {current()}"
          assert profile() == B, profile()
          assert pids(UNITS) == before
          assert boot_default() == B, f"default boot entry is {boot_default()}, not B"

      with subtest("switch activates B and keeps it the default"):
          ekapkgs(f"switch {B}")
          assert current() == B and profile() == B
          assert boot_default() == B, boot_default()
          machine.succeed("systemctl is-active e2e-added.service")
          print(machine.succeed(f"nix-env --profile {PROFILE} --list-generations"))

      with subtest("rollback returns to A"):
          ekapkgs("rollback")
          assert current() == A and profile() == A
          assert boot_default() == A, boot_default()
          marked = [l for l in ekapkgs("list-generations").splitlines() if l.endswith("(current)")]
          assert len(marked) == 1 and A in marked[0], marked
          machine.succeed("systemctl is-active e2e-removed.service")

      machine.shutdown()
    '';
}
