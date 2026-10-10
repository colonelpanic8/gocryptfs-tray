# gocryptfs-tray

A system tray icon and CLI for [gocryptfs](https://github.com/rfjakob/gocryptfs)
vaults, built for keeping an encrypted folder inside a sync tool such as
Syncthing.

- **Unlock only on request, with a PIN.** The vault password is the first line
  of a secret command (e.g. `pass show vaults/private`) followed by a PIN that
  is never stored. Neither half unlocks the vault alone.
- **A loud reminder.** While any vault is unlocked, the tray icon is a red
  badge with an open padlock and the item reports `NeedsAttention`. The icon is
  rendered in code, so it is red in every icon theme.
- **Lock from the menu only.** Left click unlocks, or opens the folder if the
  vault is already unlocked. Locking needs a deliberate menu click, so a stray
  click cannot yank the vault away from open files.
- **Ciphertext owned by another user.** With the systemd backend, gocryptfs runs
  as e.g. `syncthing`, so every encrypted file stays writable by the sync
  daemon, while the decrypted view belongs to you alone.
- **No plaintext in the sync folder.** The NixOS module can make the ciphertext
  unwritable for local users and add a `.stignore` block so Syncthing neither
  sends nor accepts names gocryptfs could not have produced.

## Usage

```
gocryptfs-tray tray                # run the tray icon
gocryptfs-tray status
gocryptfs-tray unlock [VAULT]      # alias: mount
gocryptfs-tray lock [VAULT]        # alias: unmount
gocryptfs-tray change-pin [VAULT]  # an empty current PIN means none is set yet
gocryptfs-tray init [VAULT]        # create a new vault
```

The CLI reads PINs from the terminal, or one line per PIN from stdin when stdin
is not a terminal. The tray asks through `pinentry`.

## Configuration

The config is read from `--config`, `$GOCRYPTFS_TRAY_CONFIG`,
`~/.config/gocryptfs-tray/config.toml`, then `/etc/gocryptfs-tray/config.toml`.

```toml
# pinentry program for the tray (default: `pinentry` on PATH)
pinentry = "/run/current-system/sw/bin/pinentry-gnome3"

[vaults.private]
cipher_dir = "~/Sync/Private.encrypted"    # encrypted destination
mount_dir = "~/Private"                    # where the decrypted view appears
secret_command = ["pass", "show", "vaults/private"]
pin = true                                 # default
min_pin_length = 4                         # default
backend = { type = "direct", gocryptfs_args = ["-sharedstorage"] }
```

Optional per-vault keys: `link` (the path to open, if different from
`mount_dir`) and `owner` (only show the vault to that user).

The `direct` backend runs gocryptfs as you. The `systemd` backend hands the
password to system units through a group-readable file in `handoff_dir`. The
units delete it as soon as gocryptfs has read it. The NixOS module generates
this backend's configuration.

## NixOS module

```nix
{
  inputs.gocryptfs-tray.url = "github:colonelpanic8/gocryptfs-tray";

  outputs = {nixpkgs, gocryptfs-tray, ...}: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      modules = [
        gocryptfs-tray.nixosModules.default
        {
          services.gocryptfs-tray.vaults.private = {
            user = "alice";
            cipherUser = "syncthing";
            cipherDir = "/var/lib/syncthing/sync/Private.encrypted";
            link = "/home/alice/Private";
            secretCommand = ["pass" "show" "vaults/private"];
            gocryptfsArgs = ["-sharedstorage"];
            syncthingIgnore.folder = "/var/lib/syncthing/sync";
          };
        }
      ];
    };
  };
}
```

For each vault the module creates:

- `gocryptfs-tray-<name>.service`, which mounts the vault as `cipherUser`, plus
  `-init` and `-passwd` units. A polkit rule lets `user`, and only `user`,
  start and stop them, so no `sudo` is needed.
- a group containing only `user` and `cipherUser`. It gates the decrypted view
  and the password handoff directory.
- the mount point (by default `/run/gocryptfs-tray/<name>/mnt`) and `link`
  pointing to it.
- optionally, the ciphertext lockdown and Syncthing ignore block described
  above.
- a `gocryptfs-tray` user service running the tray in `user`'s graphical
  session.

Create the secret first (e.g. `pass generate vaults/private 40`), then run
`gocryptfs-tray init` once on one machine and let the sync tool distribute the
vault.

## Security notes

- The PIN only adds protection if the secret leaks. Then its strength against
  offline guessing is the PIN's length, slowed by gocryptfs's scrypt, so prefer
  6+ characters.
- The password exists on disk only in the handoff file, under `/run` (tmpfs),
  from just before the unit starts until gocryptfs has read it.
- Root, and anyone in `wheel` with passwordless sudo, can always reach an
  unlocked vault.
- Concurrent edits of the same file on two machines produce sync conflict
  copies with undecryptable names. gocryptfs hides them.

## Development

```
direnv allow
just check          # fmt, clippy, unit tests
nix flake check     # package, NixOS module evaluation, NixOS VM test
```

## License

MIT or Apache-2.0, at your option.
