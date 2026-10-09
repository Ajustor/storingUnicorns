#!/bin/sh
# Install storingUnicorns on Linux (x86_64) or macOS (Apple Silicon):
#
#   curl -fsSL https://ajustor.github.io/storingUnicorns/install.sh | sh
#
# Installs into ~/.local/bin (override with STORINGUNICORNS_INSTALL_DIR).
# The download is checked against the SHA-256 published with the release.
set -eu

BASE="https://ajustor.github.io/storingUnicorns"
DIR="${STORINGUNICORNS_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ASSET="storingUnicorns-linux-x64" ;;
  Darwin-arm64) ASSET="storingUnicorns-macos-arm64" ;;
  *) echo "Plateforme non prise en charge : $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$BASE/SHA256SUMS" -o "$tmp/SHA256SUMS"
line=$(grep "/$ASSET\$" "$tmp/SHA256SUMS" || true)
if [ -z "$line" ]; then
  echo "Aucun binaire $ASSET dans la dernière version." >&2
  exit 1
fi
expected=${line%% *}
path=${line##* }

echo "Téléchargement de $path…"
curl -fL --progress-bar "$BASE/$path" -o "$tmp/$ASSET"

if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$ASSET" | cut -d' ' -f1)
else
  actual=$(shasum -a 256 "$tmp/$ASSET" | cut -d' ' -f1)
fi
if [ "$actual" != "$expected" ]; then
  echo "Empreinte SHA-256 incorrecte (attendu $expected, obtenu $actual)." >&2
  exit 1
fi

mkdir -p "$DIR"
install -m 755 "$tmp/$ASSET" "$DIR/storingUnicorns"
echo "storingUnicorns installé dans $DIR/storingUnicorns"

case ":$PATH:" in
  *":$DIR:"*) echo "Lancez : storingUnicorns   (ou storingUnicorns tui)" ;;
  *)
    echo ""
    echo "$DIR n'est pas dans votre PATH. Ajoutez à votre ~/.bashrc ou ~/.zshrc :"
    echo "    export PATH=\"$DIR:\$PATH\""
    ;;
esac
