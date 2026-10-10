{
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkOption types;
  cfg = config.services.gocryptfs-tray;
  runtimeRoot = "/run/gocryptfs-tray";

  vaultModule = {
    name,
    config,
    ...
  }: {
    options = {
      user = mkOption {
        type = types.str;
        description = "User who unlocks the vault and owns the decrypted files.";
      };
      cipherUser = mkOption {
        type = types.str;
        default = config.user;
        defaultText = lib.literalExpression "user";
        example = "syncthing";
        description = ''
          User that owns the ciphertext and runs gocryptfs. Set this to e.g.
          `syncthing` when another service writes the encrypted files, so they
          stay writable by it.
        '';
      };
      group = mkOption {
        type = types.str;
        default = "gocryptfs-tray-${name}";
        description = "Group containing only `user` and `cipherUser`; it gates the decrypted view.";
      };
      cipherDir = mkOption {
        type = types.str;
        description = "Encrypted gocryptfs directory.";
      };
      mountDir = mkOption {
        type = types.str;
        default = "${runtimeRoot}/${name}/mnt";
        description = "Where the decrypted view is mounted. `cipherUser` must be able to reach it.";
      };
      link = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/home/alice/Private";
        description = ''
          Symlink to `mountDir`. It is only created over an absent path, a
          symlink, or an empty directory.
        '';
      };
      secretCommand = mkOption {
        type = types.nullOr (types.listOf types.str);
        default = null;
        example = ["pass" "show" "vaults/private"];
        description = "Command whose first output line is prepended to the PIN to form the password.";
      };
      pin = mkOption {
        type = types.bool;
        default = true;
        description = "Whether unlocking asks for a PIN.";
      };
      minPinLength = mkOption {
        type = types.ints.positive;
        default = 4;
        description = "Minimum length enforced when setting a PIN.";
      };
      autoLockMinutes = mkOption {
        type = types.nullOr types.ints.positive;
        default = null;
        example = 30;
        description = ''
          Default auto-lock timeout in idle minutes, enforced while the tray
          runs. The tray menu can override it per user.
        '';
      };
      gocryptfsArgs = mkOption {
        type = types.listOf types.str;
        default = [];
        example = ["-sharedstorage"];
        description = "Extra arguments for the gocryptfs mount.";
      };
      lockdownCipherDir = mkOption {
        type = types.bool;
        default = true;
        description = ''
          On activation, make the ciphertext writable only by `cipherUser`
          and readable only by `group`, so nothing local can drop plaintext
          into it.
        '';
      };
      activationDeps = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Activation scripts to run first, e.g. ones that reset permissions under `cipherDir`.";
      };
      syncthingIgnore.folder = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/var/lib/syncthing/sync";
        description = ''
          Root of the syncthing folder containing `cipherDir`. When set, a
          managed block at the top of its `.stignore` makes syncthing neither
          send nor accept names under `cipherDir` that gocryptfs could not
          have produced.
        '';
      };
    };
  };

  unitBase = name: "gocryptfs-tray-${name}";
  handoffDir = name: "${runtimeRoot}/${name}";

  vaultSettings = name: vault:
    {
      cipher_dir = vault.cipherDir;
      mount_dir = vault.mountDir;
      owner = vault.user;
      pin = vault.pin;
      min_pin_length = vault.minPinLength;
      backend = {
        type = "systemd";
        mount_unit = "${unitBase name}.service";
        init_unit = "${unitBase name}-init.service";
        passwd_unit = "${unitBase name}-passwd.service";
        handoff_dir = handoffDir name;
      };
    }
    // lib.optionalAttrs (vault.link != null) {inherit (vault) link;}
    // lib.optionalAttrs (vault.autoLockMinutes != null) {auto_lock_minutes = vault.autoLockMinutes;}
    // lib.optionalAttrs (vault.secretCommand != null) {secret_command = vault.secretCommand;};

  configFile = (pkgs.formats.toml {}).generate "gocryptfs-tray.toml" (
    {vaults = lib.mapAttrs vaultSettings cfg.vaults;}
    // lib.optionalAttrs (cfg.pinentry != null) {inherit (cfg) pinentry;}
  );

  # gocryptfs names are base64url, at least 22 characters, or start with "gocryptfs.".
  # syncthing's glob library allows only one range or plain list per character class.
  stignoreBlock = name: vault: let
    relative = lib.removePrefix "${vault.syncthingIgnore.folder}/" vault.cipherDir;
    base64urlChars = "-ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_";
    shortName = n: let
      chars = lib.concatStrings (lib.genList (_: "?") n);
    in [
      "/${relative}/${chars}"
      "/${relative}/**/${chars}"
      "/${relative}/${chars}/**"
      "/${relative}/**/${chars}/**"
    ];
  in
    pkgs.writeText "gocryptfs-tray-${name}.stignore" (lib.concatLines (
      [
        "// BEGIN gocryptfs-tray:${name} (managed by NixOS)"
        "!/${relative}/gocryptfs.*"
        "!/${relative}/**/gocryptfs.*"
        "/${relative}/**[!${base64urlChars}/]**"
      ]
      ++ lib.concatMap shortName (lib.range 1 21)
      ++ ["// END gocryptfs-tray:${name}"]
    ));

  activationScript = name: vault: let
    q = lib.escapeShellArg;
    folder = vault.syncthingIgnore.folder;
  in ''
    install -d -o ${q vault.cipherUser} ${q vault.cipherDir}
    ${lib.optionalString vault.lockdownCipherDir ''
      chmod -R g-w,o= ${q vault.cipherDir}
      chgrp ${q vault.group} ${q vault.cipherDir}
      chmod g+rx ${q vault.cipherDir}
    ''}
    ${lib.optionalString (folder != null) ''
      stignore=${q "${folder}/.stignore"}
      stignore_new="$(mktemp)"
      cat ${stignoreBlock name vault} > "$stignore_new"
      if [ -f "$stignore" ]; then
        ${pkgs.gnused}/bin/sed '/^\/\/ BEGIN gocryptfs-tray:${name} /,/^\/\/ END gocryptfs-tray:${name}$/d' "$stignore" >> "$stignore_new"
      fi
      if ! ${pkgs.diffutils}/bin/cmp -s "$stignore_new" "$stignore"; then
        install -m 0660 "$stignore_new" "$stignore"
        chown --reference=${q folder} "$stignore"
      fi
      rm -f "$stignore_new"
    ''}
    ${lib.optionalString (vault.link != null) ''
      link=${q vault.link}
      if [ -d "$link" ] && [ ! -L "$link" ]; then
        rmdir "$link" 2>/dev/null || echo "gocryptfs-tray: $link is a non-empty directory; not linking the ${name} vault" >&2
      fi
      if [ ! -e "$link" ] || [ -L "$link" ]; then
        ln -sfn ${q vault.mountDir} "$link"
        chown -h ${q vault.user} "$link"
      fi
    ''}
  '';

  unitsFor = name: vault: let
    q = lib.escapeShellArg;
    handoff = handoffDir name;
    rm = "+${pkgs.coreutils}/bin/rm -f";
  in {
    "${unitBase name}" = {
      description = "Decrypted view of the ${name} gocryptfs vault";
      unitConfig.ConditionPathExists = "${vault.cipherDir}/gocryptfs.conf";
      path = ["/run/wrappers" pkgs.gocryptfs pkgs.coreutils pkgs.getent pkgs.util-linux];
      script = ''
        exec gocryptfs -q -allow_other \
          -force_owner "$(id -u ${q vault.user}):$(getent group ${q vault.group} | cut -d: -f3)" \
          ${lib.escapeShellArgs vault.gocryptfsArgs} \
          -passfile ${handoff}/passphrase -- ${q vault.cipherDir} ${q vault.mountDir}
      '';
      serviceConfig = {
        Type = "forking";
        User = vault.cipherUser;
        ExecStartPost = "${rm} ${handoff}/passphrase";
        ExecStopPost = [
          "${rm} ${handoff}/passphrase"
          "-+${pkgs.util-linux}/bin/umount -l ${q vault.mountDir}"
        ];
      };
    };
    "${unitBase name}-init" = {
      description = "Create the ${name} gocryptfs vault";
      path = [pkgs.gocryptfs];
      script = "exec gocryptfs -q -init -passfile ${handoff}/passphrase.new -- ${q vault.cipherDir}";
      serviceConfig = {
        Type = "oneshot";
        User = vault.cipherUser;
        ExecStopPost = "${rm} ${handoff}/passphrase.new";
      };
    };
    "${unitBase name}-passwd" = {
      description = "Change the password of the ${name} gocryptfs vault";
      path = [pkgs.gocryptfs];
      script = "exec gocryptfs -q -passwd -passfile ${handoff}/passphrase.old -- ${q vault.cipherDir} < ${handoff}/passphrase.new";
      serviceConfig = {
        Type = "oneshot";
        User = vault.cipherUser;
        ExecStopPost = "${rm} ${handoff}/passphrase.old ${handoff}/passphrase.new";
      };
    };
  };

  owners = lib.unique (lib.mapAttrsToList (_: vault: vault.user) cfg.vaults);
  unitOwners = lib.listToAttrs (lib.concatLists (lib.mapAttrsToList (name: vault:
    map (suffix: lib.nameValuePair "${unitBase name}${suffix}.service" vault.user) ["" "-init" "-passwd"])
  cfg.vaults));
in {
  options.services.gocryptfs-tray = {
    package = mkOption {
      type = types.package;
      description = "The gocryptfs-tray package.";
    };
    pinentry = mkOption {
      type = types.nullOr types.str;
      default = "${pkgs.pinentry-gnome3}/bin/pinentry-gnome3";
      defaultText = lib.literalExpression ''"''${pkgs.pinentry-gnome3}/bin/pinentry-gnome3"'';
      description = "pinentry program the tray uses to ask for PINs; null uses `pinentry` from PATH.";
    };
    tray.enable = mkOption {
      type = types.bool;
      default = true;
      description = "Run the tray icon as a user service for each vault owner's graphical session.";
    };
    vaults = mkOption {
      type = types.attrsOf (types.submodule vaultModule);
      default = {};
      description = "gocryptfs vaults to manage.";
    };
  };

  config = lib.mkIf (cfg.vaults != {}) {
    assertions =
      lib.mapAttrsToList (name: vault: {
        assertion = vault.syncthingIgnore.folder == null || lib.hasPrefix "${vault.syncthingIgnore.folder}/" vault.cipherDir;
        message = "services.gocryptfs-tray.vaults.${name}: cipherDir must be inside syncthingIgnore.folder";
      })
      cfg.vaults;

    environment.systemPackages = [cfg.package];
    environment.etc."gocryptfs-tray/config.toml".source = configFile;

    # The mount runs as cipherUser; allow_other lets the plaintext owner see it.
    programs.fuse = {
      enable = true;
      userAllowOther = true;
    };

    users.groups = lib.mapAttrs' (_: vault:
      lib.nameValuePair vault.group {members = lib.unique [vault.user vault.cipherUser];})
    cfg.vaults;

    systemd.tmpfiles.rules =
      ["d ${runtimeRoot} 0755 root root -"]
      ++ lib.concatLists (lib.mapAttrsToList (name: vault: [
          "d ${handoffDir name} 2750 ${vault.user} ${vault.group} -"
          "d ${vault.mountDir} 0750 ${vault.cipherUser} ${vault.group} -"
        ])
        cfg.vaults);

    systemd.services = lib.mkMerge (lib.mapAttrsToList unitsFor cfg.vaults);

    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        var owners = ${builtins.toJSON unitOwners};
        if (action.id == "org.freedesktop.systemd1.manage-units" &&
            owners[action.lookup("unit")] === subject.user &&
            ["start", "stop", "restart"].indexOf(action.lookup("verb")) >= 0) {
          return polkit.Result.YES;
        }
      });
    '';

    system.activationScripts = lib.mapAttrs' (name: vault:
      lib.nameValuePair "gocryptfs-tray-${name}" {
        deps = ["users" "groups"] ++ vault.activationDeps;
        text = activationScript name vault;
      })
    cfg.vaults;

    systemd.user.services.gocryptfs-tray = lib.mkIf cfg.tray.enable {
      description = "Tray icon for gocryptfs vaults";
      wantedBy = ["graphical-session.target"];
      partOf = ["graphical-session.target"];
      after = ["graphical-session.target"];
      unitConfig.ConditionUser = map (user: "|${user}") owners;
      # The secret command (e.g. pass) and systemctl come from the user's profile.
      environment.PATH = lib.mkForce "/run/wrappers/bin:/etc/profiles/per-user/%u/bin:/run/current-system/sw/bin";
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} tray";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}
