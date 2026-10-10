# gocryptfs-tray

Tray icon (StatusNotifierItem via `ksni`) and CLI for gocryptfs vaults whose
password is a secret command's output plus a PIN.

## Layout

- `src/config.rs`: TOML config; vaults and the `direct`/`systemd` backends.
- `src/vault.rs`: unlock, lock, change-pin and init for both backends. Wrong
  passwords surface as `WrongPassword` (gocryptfs exit code 12).
- `src/mounts.rs`: mount detection from `/proc/self/mountinfo`, plus a `POLLPRI`
  watcher that the tray uses instead of polling.
- `src/icons.rs`: padlock icons rendered in code (red badge when unlocked).
- `src/tray.rs`: the tray. Menu callbacks only enqueue actions; a worker thread
  runs pinentry and the vault operations so the tray never blocks.
- `src/prompt.rs`: terminal and pinentry PIN prompts.
- `nix/module.nix`: NixOS module (units, group, polkit rule, tmpfiles, ciphertext
  lockdown, Syncthing `.stignore` block, link, tray user service).
- `nix/vm-test.nix`: NixOS VM test of the module and CLI end to end.

## Invariants

- The PIN is never stored. Passwords only touch disk in `systemd` backend
  handoff files under `/run`, which `Handoff` and the units both delete.
- With the `systemd` backend, gocryptfs runs as `cipherUser`. The mount root
  mirrors the cipher dir's mode and is presented as `user:group` via
  `-force_owner`, so `group` must be able to read and search the cipher dir.
  Otherwise gocryptfs cannot access its own mount.
- gocryptfs ignores its own umask; ciphertext modes follow the caller's umask.
  Lockdown therefore relies on the cipher dir's mode, not on unit umasks.
- Syncthing globs allow one range or one plain list per character class; the
  `.stignore` patterns spell the base64url alphabet out as a list.
- Locking stays menu-only; left click must never lock.

## Checks

- `just check`: fmt (cargo + alejandra), clippy with `-D warnings`, unit tests.
- `nix flake check`: package build, module evaluation, and the VM test (needs
  KVM). Run the VM test after any change to `nix/module.nix` or `src/vault.rs`:
  `nix build .#checks.x86_64-linux.vm -L`.
