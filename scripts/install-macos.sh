#!/usr/bin/env bash
#
# Install (or update) the storingUnicorns app on macOS from the terminal:
#
#     curl -fsSL https://ajustor.github.io/storingUnicorns/install-macos.sh | bash
#
# Why a script: the app is signed ad hoc (no Apple Developer ID). A .dmg
# downloaded by a browser gets the quarantine attribute, so Gatekeeper calls
# the app dangerous, and macOS 26 (Tahoe) even hangs it when it sits in a
# folder named "Applications". Files downloaded by curl carry no quarantine,
# and installing outside any "Applications" folder avoids the macOS 26 block.
#
# What it does:
#   1. reads the latest release from the download site (latest.json),
#   2. downloads the disk image and checks its SHA-256,
#   3. copies "storingUnicorns.app" into ~/Apps (STORINGUNICORNS_APP_DIR to
#      change it), where Spotlight and the Finder find it,
#   4. puts the `storingUnicorns` command on the PATH, the way the app itself
#      does (src/cli_link.rs): /usr/local/bin when writable, else ~/.local/bin,
#      added to the shell's startup file when missing.
#
# Run it again to update; the in-app updater keeps it current afterwards.
# scripts/install.sh installs the bare binary instead (command line only).
set -euo pipefail

BASE_URL="${STORINGUNICORNS_BASE_URL:-https://ajustor.github.io/storingUnicorns}"
INSTALL_DIR="${STORINGUNICORNS_APP_DIR:-$HOME/Apps}"
NAME="storingUnicorns"
EXE="storingUnicorns"
ASSET="storingUnicorns-macos-arm64.dmg"
# Same marker as src/cli_link.rs, so the app recognises the line and never adds it twice.
PROFILE_MARKER='# Added by storingUnicorns: the `storingUnicorns` command'

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'Erreur : %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || die "ce script est réservé à macOS (sous Linux : install.sh)."
[ "$(uname -m)" = arm64 ] || die "seuls les Mac Apple Silicon (M1 et suivants) sont pris en charge."
case "$INSTALL_DIR" in
  */Applications | */Applications/*)
    die "macOS 26 bloque l'app dans tout dossier nommé « Applications » : choisissez un autre STORINGUNICORNS_APP_DIR." ;;
esac

work=$(mktemp -d)
mount=""
cleanup() {
  [ -n "$mount" ] && hdiutil detach -quiet "$mount" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

# --- Find the latest release -------------------------------------------------
say "Recherche de la dernière version"
curl -fsSL "$BASE_URL/latest.json" -o "$work/latest.json" ||
  die "impossible de joindre $BASE_URL/latest.json"
# plutil reads JSON too: no python or jq needed on a fresh Mac.
field() { plutil -extract "$1" raw -o - "$work/latest.json" 2>/dev/null; }
version=$(field version) || die "latest.json n'indique pas de version"
url="" sha=""
i=0
while name=$(field "assets.$i.name"); do
  if [ "$name" = "$ASSET" ]; then
    url=$(field "assets.$i.url")
    sha=$(field "assets.$i.sha256")
    break
  fi
  i=$((i + 1))
done
[ -n "$url" ] || die "la version $version n'a pas de $ASSET"

# --- Download and check ------------------------------------------------------
say "Téléchargement de storingUnicorns $version"
curl -fL --progress-bar "$url" -o "$work/$ASSET"
actual=$(shasum -a 256 "$work/$ASSET" | cut -d' ' -f1)
[ "$actual" = "$sha" ] || die "empreinte SHA-256 incorrecte pour $ASSET (attendu $sha, obtenu $actual)"

# --- Install the app ---------------------------------------------------------
if pgrep -x "$EXE" > /dev/null; then
  say "storingUnicorns est ouvert : quittez-le pour utiliser la nouvelle version"
fi
mount="$work/mnt"
mkdir "$mount"
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mount" "$work/$ASSET"
app="$INSTALL_DIR/$NAME.app"
say "Installation dans $app"
mkdir -p "$INSTALL_DIR"
rm -rf "$app"
ditto "$mount/$NAME.app" "$app"
hdiutil detach -quiet "$mount" && mount=""
# Nothing above sets it, but an older copy might have kept it.
xattr -dr com.apple.quarantine "$app" 2>/dev/null || true

# --- Put `storingUnicorns` on the PATH ---------------------------------------
target="$app/Contents/MacOS/$EXE"
if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
  bin_dir=/usr/local/bin
else
  bin_dir="$HOME/.local/bin"
  mkdir -p "$bin_dir"
fi
link="$bin_dir/$EXE"
# Like src/cli_link.rs: a command that isn't ours (a file, such as the binary
# installed by install.sh, or a live link to another program) is never replaced.
ours=true
if [ -L "$link" ]; then
  current=$(readlink "$link")
  case "$(basename "$current")" in
    storingUnicorns*) ;;
    *) [ -e "$link" ] && ours=false ;;
  esac
elif [ -e "$link" ]; then
  ours=false
fi
if $ours; then
  ln -sfn "$target" "$link"
  say "Lien $link -> $target"
else
  say "$link existe déjà (binaire seul ou autre programme) : laissé tel quel"
fi

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *)
    case "$(basename "${SHELL:-}")" in
      zsh) profile="$HOME/.zshrc" line="export PATH=\"$bin_dir:\$PATH\"" ;;
      bash) profile="$HOME/.bash_profile" line="export PATH=\"$bin_dir:\$PATH\"" ;;
      fish) profile="$HOME/.config/fish/conf.d/storingUnicorns.fish"
        line="fish_add_path --global \"$bin_dir\"" ;;
      *) profile="" line="" ;;
    esac
    if [ -z "$profile" ]; then
      say "Ajoutez $bin_dir à votre PATH pour utiliser storingUnicorns"
    elif ! grep -qxF "$line" "$profile" 2>/dev/null; then
      mkdir -p "$(dirname "$profile")"
      printf '\n%s\n%s\n' "$PROFILE_MARKER" "$line" >> "$profile"
      say "$bin_dir ajouté au PATH dans $profile (ouvrez un nouveau terminal)"
    fi
    ;;
esac

# A copy dragged into /Applications from the .dmg hangs on macOS 26.
if [ -d "/Applications/$NAME.app" ]; then
  say "Une ancienne copie reste dans /Applications : vous pouvez la supprimer"
fi

say "Terminé. Ouvrez storingUnicorns depuis Spotlight, ou lancez : storingUnicorns"
