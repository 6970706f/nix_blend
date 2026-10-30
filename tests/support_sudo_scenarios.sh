#!/bin/sh
# Exercises the elevated code paths, as `sudo` would run them. It must run inside
# a user and mount namespace (see tests/sudo.rs), where "root" is the unprivileged
# user outside it. /etc/passwd, /etc/nixos and the system profile are replaced by
# private mounts, so nothing here reaches the real system.
set -u
BIN_SRC="$1"
ORIGINAL_PATH="$PATH"
SH=$(command -v sh)
W=/var/tmp
FAILURES=0

fail() { echo "FAIL: $1"; FAILURES=$((FAILURES + 1)); }
check() { # description, expected, actual
    [ "$2" = "$3" ] || fail "$1: expected '$2', got '$3'"
}
contains() { # description, needle, haystack
    case "$3" in *"$2"*) ;; *) fail "$1: '$2' not in: $3" ;; esac
}
lacks() { # description, needle, haystack
    case "$3" in *"$2"*) fail "$1: unexpected '$2' in: $3" ;; esac
}

mount -t tmpfs tmpfs "$W" || exit 90
chmod 755 "$W"
mkdir -p "$W/bin" "$W/evil" "$W/trusted" "$W/secret" "$W/etc" "$W/nixos" "$W/rh"
chmod 755 "$W/bin" "$W/evil" "$W/trusted" "$W/nixos"
chmod 700 "$W/secret"
cp "$BIN_SRC" "$W/bin/nixblend"
touch "$W/tools.log"; chmod 666 "$W/tools.log"

printf 'root:x:0:0::%s/rh:/bin/sh\nalice:x:1000:100::%s/home/alice:/bin/sh\n' "$W" "$W" > "$W/etc/passwd"
mount --bind "$W/etc/passwd" /etc/passwd || exit 91
mount --bind "$W/nixos" /etc/nixos || exit 92

cp -a /run/current-system/sw/bin/. "$W/trusted/" || exit 93
for tool in nix nix-instantiate nixos-rebuild; do rm -f "$W/trusted/$tool"; done
cat > "$W/trusted/nix" <<EOF
#!$SH
echo "nix uid=\$(id -u) HOME=\$HOME \$*" >> $W/tools.log
meta() { echo "{\"\$1\":{\"name\":\"\$1-1\",\"pname\":\"\$1\",\"version\":\"1\",\"description\":\"d\",\"available\":\$2,\"broken\":false,\"unfree\":\$3,\"insecure\":false}}"; }
case "\$*" in
  *attrNames*) echo '[]' ;;
  *" eval "*)
    case "\$*" in
      *"p.ripgrep or"*) meta ripgrep true false ;;
      *"p.blocked or"*) meta blocked false true ;;
      *"p.overlaid or"*) meta overlaid true false ;;
      *) echo '{}' ;;
    esac ;;
  *"#"*) exit 1 ;;
  *) echo '{}' ;;
esac
EOF
cat > "$W/trusted/nix-instantiate" <<EOF
#!$SH
echo "nix-instantiate uid=\$(id -u)" >> $W/tools.log
EOF
cat > "$W/trusted/nixos-rebuild" <<EOF
#!$SH
echo "nixos-rebuild uid=\$(id -u) \$*" >> $W/tools.log
EOF
for tool in nix nix-instantiate nixos-rebuild; do
    cat > "$W/evil/$tool" <<EOF
#!$SH
echo "evil \$(id -u)" > $W/proof
exit 0
EOF
done
chmod 755 "$W/trusted/nix" "$W/trusted/nix-instantiate" "$W/trusted/nixos-rebuild" \
    "$W/evil/nix" "$W/evil/nix-instantiate" "$W/evil/nixos-rebuild"
mount --bind "$W/trusted" /run/current-system/sw/bin || exit 94

cat > /etc/nixos/flake.lock <<EOF
{"nodes":{"a":{"locked":{"type":"github","owner":"NixOS","repo":"nixpkgs","rev":"stable-rev"}},
"b":{"locked":{"type":"github","owner":"NixOS","repo":"nixpkgs","rev":"unstable-rev"}},
"root":{"inputs":{"nixpkgs":"a","nixpkgs-unstable":"b"}}},"root":"root","version":7}
EOF
echo '{ }' > /etc/nixos/flake.nix

HOME_DIR="$W/home/alice"
CONFIG_DIR="$HOME_DIR/.local/share/nixblend"
LIST='{ pkgs, ... }:\n{\n  home.packages = with pkgs; [\n    git\n  ];\n}\n'

reset() {
    rm -rf "${W:?}/home" "${W:?}/proof"
    : > "$W/tools.log"
    mkdir -p "$HOME_DIR"
    chown -R 1000:100 "$W/home"
}
as_alice() { # file written as the user would
    printf "$LIST" > "$1"
    chown 1000:100 "$1"
}
set_config() {
    mkdir -p "$CONFIG_DIR"
    printf 'packages_file = "%s"\n' "$1" > "$CONFIG_DIR/config.toml"
    chown -R 1000:100 "$HOME_DIR/.local"
}
run() { # input, package
    printf "$1" | env -i PATH="$W/evil:$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 \
        "$W/bin/nixblend" install "$2" 2>&1
}

# First run: config belongs to the user, the file keeps its owner and mode, each
# tool runs with the expected identity, and PATH does not choose the tools.
reset
as_alice "$HOME_DIR/p.nix"; chmod 640 "$HOME_DIR/p.nix"
out=$(run "$HOME_DIR/p.nix\ny\ny\n" ripgrep)
contains "first run" "Rebuild: done" "$out"
check "config owner" "1000:100 600" "$(stat -c '%u:%g %a' "$CONFIG_DIR/config.toml")"
check "config directory owner" "1000:100 700" "$(stat -c '%u:%g %a' "$CONFIG_DIR")"
check "file owner and mode" "1000:100 640" "$(stat -c '%u:%g %a' "$HOME_DIR/p.nix")"
contains "edit applied" "ripgrep" "$(cat "$HOME_DIR/p.nix")"
[ -e "$W/proof" ] && fail "a tool from the caller's PATH ran"
log=$(cat "$W/tools.log")
contains "search as the user" "nix uid=1000 HOME=$HOME_DIR" "$log"
contains "metadata uses the system configuration" "/etc/nixos#nixosConfigurations.nixos.pkgs" "$log"
contains "syntax check as the user" "nix-instantiate uid=1000" "$log"
contains "rebuild as root" "nixos-rebuild uid=0 switch --flake /etc/nixos#nixos" "$log"

# Root-owned file under the system configuration: root privileges are used.
reset
printf "$LIST" > /etc/nixos/packages.nix; chmod 600 /etc/nixos/packages.nix
set_config /etc/nixos/packages.nix
out=$(run 'y\nn\n' ripgrep)
contains "system file edited" "Edit: package 'ripgrep' added" "$out"
check "system file owner" "0:0 600" "$(stat -c '%u:%g %a' /etc/nixos/packages.nix)"
contains "system file content" "ripgrep" "$(cat /etc/nixos/packages.nix)"
contains "syntax check never needs root" "nix-instantiate uid=1000" "$(cat "$W/tools.log")"
rm -f /etc/nixos/packages.nix

# User-owned config pointing at a root-only file elsewhere: not read, not changed.
reset
printf '{ pkgs }: with pkgs; [\n  # password = hunter2\n]\n' > "$W/secret/root.nix"; chmod 600 "$W/secret/root.nix"
before=$(cat "$W/secret/root.nix")
ln -s "$W/secret/root.nix" "$HOME_DIR/link.nix"; chown -h 1000:100 "$HOME_DIR/link.nix"
set_config "$HOME_DIR/link.nix"
out=$(run 'y\nn\n' ripgrep)
lacks "secret through symlink" "hunter2" "$out"
check "secret unchanged" "$before" "$(cat "$W/secret/root.nix")"
contains "secret refused" "edit error" "$out"

set_config "$W/secret/root.nix"
out=$(run 'y\nn\n' ripgrep)
lacks "secret by path" "hunter2" "$out"
check "secret unchanged by path" "$before" "$(cat "$W/secret/root.nix")"

# A root-owned, world-readable file outside /etc/nixos is not edited either.
reset
printf "$LIST" > "$W/public.nix"; chmod 644 "$W/public.nix"
set_config "$W/public.nix"
out=$(run 'y\nn\n' ripgrep)
contains "outside system config" "only used for files under /etc/nixos" "$out"
lacks "outside unchanged" "ripgrep" "$(cat "$W/public.nix")"

# A .nix link inside the system configuration cannot reach a root-only text file.
reset
printf '{ pkgs }: with pkgs; [ ]\n' > "$W/secret/shadow"; chmod 600 "$W/secret/shadow"
ln -s "$W/secret/shadow" /etc/nixos/escape.nix
set_config /etc/nixos/escape.nix
out=$(run 'y\nn\n' ripgrep)
contains "escape refused" "edit error" "$out"
check "escape unchanged" "{ pkgs }: with pkgs; [ ]" "$(cat "$W/secret/shadow")"
rm -f /etc/nixos/escape.nix

# Symlinks in the home cannot lead root to write the config elsewhere.
reset
mkdir -p "$W/rootonly" "$HOME_DIR/.local"
ln -s "$W/rootonly" "$HOME_DIR/.local/share"
chown -h 1000:100 "$HOME_DIR/.local" "$HOME_DIR/.local/share"
as_alice "$HOME_DIR/p.nix"
out=$(run "$HOME_DIR/p.nix\nn\n" ripgrep)
contains "config through symlink" "config error" "$out"
check "nothing written through symlink" "" "$(ls -A "$W/rootonly")"

# Mismatching SUDO_UID.
reset
out=$(printf '' | env -i PATH="$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=0 "$W/bin/nixblend" install ripgrep 2>&1)
contains "SUDO_UID mismatch" "SUDO_UID does not match" "$out"

# A package the configuration refuses needs a decision.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
out=$(run 'n\n' blocked)
contains "unavailable warning" "unfree, and not allowed" "$out"
lacks "unavailable refused" "blocked" "$(cat "$HOME_DIR/p.nix")"
out=$(run 'y\ny\nn\n' blocked)
contains "unavailable accepted" "Edit: package 'blocked' added" "$out"

# Packages only an overlay provides are found through the configuration.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
out=$(run 'y\nn\n' overlaid)
contains "overlay package" "Edit: package 'overlaid' added" "$out"

# A channel alias searches the revision locked for that input.
reset
printf '{ pkgs, inputs, ... }:\nlet\n  unstable = import inputs.nixpkgs-unstable { };\nin {\n  home.packages = with pkgs; [\n    git\n  ];\n}\n' > "$HOME_DIR/p.nix"
chown 1000:100 "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
out=$(run 'y\nn\n' unstable.ripgrep)
contains "channel package" "Edit: package 'unstable.ripgrep' added" "$out"
contains "channel entry" "    unstable.ripgrep" "$(cat "$HOME_DIR/p.nix")"
contains "channel revision" "github:NixOS/nixpkgs/unstable-rev#legacyPackages" "$(cat "$W/tools.log")"

# An unknown input is a validation failure, not a missing package.
reset
printf '{ pkgs, inputs, ... }:\nlet\n  other = import inputs.not-locked { };\nin {\n  home.packages = with pkgs; [ ];\n}\n' > "$HOME_DIR/p.nix"
chown 1000:100 "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
out=$(run 'y\n' other.ripgrep)
contains "unknown input" "could not validate" "$out"

# A full search uses the locked revision, as the user.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
run 'y\n' nothing-like-this > /dev/null
log=$(cat "$W/tools.log")
contains "full search revision" "search --json github:NixOS/nixpkgs/stable-rev" "$log"
lacks "full search never as root" "nix uid=0" "$log"

# Removal, with the identity of the file's owner.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
out=$(printf 'y\nn\n' | env -i PATH="$W/evil:$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 "$W/bin/nixblend" remove git 2>&1)
contains "removal" "Edit: 'git' removed" "$out"
check "removal keeps the owner" "1000:100" "$(stat -c '%u:%g' "$HOME_DIR/p.nix")"
check "backup owner" "1000:100" "$(stat -c '%u:%g' "$HOME_DIR/.p.nix.nixblend-backup")"
lacks "removal content" "git" "$(cat "$HOME_DIR/p.nix")"

# Reinstalling only rebuilds, as root, and leaves the file alone.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
before=$(cat "$HOME_DIR/p.nix")
out=$(env -i PATH="$W/evil:$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 "$W/bin/nixblend" reinstall git 2>&1 </dev/null)
contains "reinstall" "Rebuild: done" "$out"
check "reinstall keeps the file" "$before" "$(cat "$HOME_DIR/p.nix")"
contains "reinstall as root" "nixos-rebuild uid=0" "$(cat "$W/tools.log")"
[ -e "$HOME_DIR/.p.nix.nixblend-backup" ] && fail "reinstall made a backup"

# Listing does not read a root-only file outside the system configuration.
reset
set_config "$W/secret/root.nix"
out=$(env -i PATH="$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 "$W/bin/nixblend" list 2>&1 </dev/null)
lacks "listing a secret" "hunter2" "$out"
contains "listing refused" "only used for files under /etc/nixos" "$out"

# A missing SUDO_UID is not trusted.
reset
out=$(env -i PATH="$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice "$W/bin/nixblend" list 2>&1 </dev/null)
contains "missing SUDO_UID" "SUDO_UID does not match" "$out"

# A world-writable config is not trusted while elevated.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"; chmod 666 "$CONFIG_DIR/config.toml"
out=$(run 'y\n' ripgrep)
contains "unsafe config" "not be writable by group or others" "$out"

# Variables that change how tools behave do not reach them while elevated.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
cat > "$W/trusted/nixos-rebuild" <<EOF
#!$SH
echo "rebuild env NIX_CONFIG=[\${NIX_CONFIG:-}] LD=[\${LD_LIBRARY_PATH:-}]" >> $W/tools.log
EOF
env -i PATH="$W/evil:$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 NIX_CONFIG=evil LD_LIBRARY_PATH=/evil "$W/bin/nixblend" reinstall git >/dev/null 2>&1 </dev/null
contains "clean environment" "NIX_CONFIG=[] LD=[]" "$(cat "$W/tools.log")"

# A variable outside the allowed list does not reach the rebuild, and HOME is root's.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
cat > "$W/trusted/nixos-rebuild" <<EOF
#!$SH
echo "rebuild env [\${PYTHONPATH:-}][\${BASH_ENV:-}][\${GIT_DIR:-}] HOME=\$HOME TERM=\${TERM:-}" >> $W/tools.log
EOF
env -i PATH="$W/evil:$ORIGINAL_PATH" HOME="$HOME_DIR" TERM=xterm SUDO_USER=alice SUDO_UID=1000 PYTHONPATH=/evil BASH_ENV=/evil GIT_DIR=/evil "$W/bin/nixblend" reinstall git >/dev/null 2>&1 </dev/null
contains "allowed environment only" "rebuild env [][][] HOME=/root TERM=xterm" "$(cat "$W/tools.log")"

# Groups of the user work under sudo, for a file of a secondary group.
printf 'root:x:0:\nusers:x:100:alice\nwheel:x:200:alice\n' > "$W/etc/group"
mount --bind "$W/etc/group" /etc/group || exit 95
reset
as_alice "$HOME_DIR/p.nix"; chown 1000:200 "$HOME_DIR/p.nix"; chmod 660 "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
out=$(run 'y\nn\n' ripgrep)
contains "secondary group" "Edit: package 'ripgrep' added" "$out"
check "secondary group kept" "1000:200 660" "$(stat -c '%u:%g %a' "$HOME_DIR/p.nix")"

# A shared file reachable only through a secondary group.
reset
mkdir -p "$W/shared"; chown 0:200 "$W/shared"; chmod 770 "$W/shared"
printf "$LIST" > "$W/shared/p.nix"; chown 0:200 "$W/shared/p.nix"; chmod 660 "$W/shared/p.nix"
set_config "$W/shared/p.nix"
out=$(env -i PATH="$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 "$W/bin/nixblend" list 2>&1 </dev/null)
contains "shared file readable" "git" "$out"
lacks "shared file not refused" "only used for files" "$out"

# Being a member of a group does not open what belongs to root's groups only.
reset
printf 'x' > "$W/secret/group0.nix"; chown 0:0 "$W/secret/group0.nix"; chmod 640 "$W/secret/group0.nix"; chmod 750 "$W/secret"
set_config "$W/secret/group0.nix"
out=$(env -i PATH="$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 "$W/bin/nixblend" list 2>&1 </dev/null)
contains "root's group is not the user's" "only used for files under /etc/nixos" "$out"
chmod 700 "$W/secret"

# A configuration that cannot be evaluated does not stop the query, and it is reported.
reset
as_alice "$HOME_DIR/p.nix"; set_config "$HOME_DIR/p.nix"
cp "$W/trusted/nix" "$W/nix.good"
cat > "$W/trusted/nix" <<EOF
#!$SH
echo "nix \$*" >> $W/tools.log
case "\$*" in
  *nixosConfigurations*) echo "error: no such configuration" >&2; exit 1 ;;
  *attrNames*) echo '[]' ;;
  *" eval "*) echo '{"ripgrep":{"name":"ripgrep-1","pname":"ripgrep","version":"1","description":"d","available":false,"broken":false,"unfree":true,"insecure":false}}' ;;
  *) echo '{}' ;;
esac
EOF
out=$(env -i PATH="$ORIGINAL_PATH" HOME="$W/rh" SUDO_USER=alice SUDO_UID=1000 "$W/bin/nixblend" info ripgrep 2>&1 </dev/null)
contains "fallback answers" "Version: 1" "$out"
contains "fallback explained" "no such configuration" "$out"
contains "fallback availability is unknown" "Status: Unknown" "$out"
contains "fallback uses the locked revision" "stable-rev#legacyPackages" "$(cat "$W/tools.log")"
cp "$W/nix.good" "$W/trusted/nix"

[ "$FAILURES" -eq 0 ] && echo "ALL SCENARIOS PASSED"
exit "$FAILURES"
