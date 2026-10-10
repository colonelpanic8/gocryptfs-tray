{
  pkgs,
  module,
}:
pkgs.testers.runNixOSTest {
  name = "gocryptfs-tray";

  nodes.machine = {
    imports = [module];
    users.users.alice = {
      isNormalUser = true;
      group = "users";
    };
    users.users.bob = {
      isNormalUser = true;
      group = "users";
    };
    users.users.syncthing = {
      isSystemUser = true;
      group = "syncthing";
    };
    users.groups.syncthing = {};
    environment.etc."vault-secret" = {
      text = "correct-horse-battery\n";
      user = "alice";
      mode = "0400";
    };
    services.gocryptfs-tray = {
      tray.enable = false;
      vaults.private = {
        user = "alice";
        cipherUser = "syncthing";
        cipherDir = "/var/lib/syncthing/sync/Private.encrypted";
        link = "/home/alice/Private";
        secretCommand = ["cat" "/etc/vault-secret"];
        gocryptfsArgs = ["-sharedstorage"];
        syncthingIgnore.folder = "/var/lib/syncthing/sync";
      };
    };
  };

  testScript = ''
    cipher = "/var/lib/syncthing/sync/Private.encrypted"

    def as_alice(command):
        return f"su - alice -c {repr(command)}"

    machine.wait_for_unit("multi-user.target")

    with subtest("init and unlock through polkit-authorized units"):
        machine.succeed(as_alice("printf '1234\\n1234\\n' | gocryptfs-tray init"))
        machine.succeed(as_alice("echo 1234 | gocryptfs-tray unlock"))
        machine.succeed(as_alice("echo hello > ~/Private/note.txt"))
        machine.succeed(as_alice("gocryptfs-tray status | grep UNLOCKED"))

    with subtest("ciphertext belongs to the cipher user and the handoff file is gone"):
        owners = machine.succeed(f"stat -c %U {cipher}/*").split()
        assert set(owners) == {"syncthing"}, owners
        machine.fail("ls /run/gocryptfs-tray/private/passphrase")

    with subtest("nobody else can read plaintext or write ciphertext"):
        machine.fail("su - bob -c 'cat /run/gocryptfs-tray/private/mnt/note.txt'")
        machine.fail(as_alice(f"touch {cipher}/plain.txt"))

    with subtest("syncthing ignore block is installed"):
        machine.succeed("grep -q 'BEGIN gocryptfs-tray:private' /var/lib/syncthing/sync/.stignore")

    with subtest("wrong PIN is rejected"):
        machine.succeed(as_alice("gocryptfs-tray lock"))
        output = machine.fail(as_alice("echo 9999 | gocryptfs-tray unlock 2>&1"))
        assert "wrong PIN" in output, output

    with subtest("change PIN"):
        machine.succeed(as_alice("printf '1234\\n5678\\n5678\\n' | gocryptfs-tray change-pin"))
        machine.fail(as_alice("echo 1234 | gocryptfs-tray unlock"))
        machine.succeed(as_alice("echo 5678 | gocryptfs-tray unlock"))
        machine.succeed(as_alice("grep -q hello ~/Private/note.txt"))
        machine.succeed(as_alice("gocryptfs-tray lock"))
        machine.fail("mountpoint -q /run/gocryptfs-tray/private/mnt")
  '';
}
