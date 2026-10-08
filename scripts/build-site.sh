#!/usr/bin/env bash
# Build the GitHub Pages site into ./site for release TAG.
#
# Usage: build-site.sh <tag> <base-url> <artifacts-dir>
#
# Copies the release files found (recursively) in <artifacts-dir>, writes
# `latest.json` (read by the in-app updater, src/updater/mod.rs) and
# `SHA256SUMS` (read by scripts/install.sh), renders the download page from
# pages/index.html and publishes the install scripts. Release notes come from
# CHANGELOG.md, falling back to the GitHub release text (needs GH_TOKEN and
# GITHUB_REPOSITORY).
set -euo pipefail

TAG="$1"
BASE_URL="$2"
ARTIFACTS="$3"

rm -rf site
dir="site/download/$TAG"
mkdir -p "$dir"
find "$ARTIFACTS" -type f ! -name '*.ico' -exec cp {} "$dir/" \;

if ! notes=$(python3 scripts/changelog.py CHANGELOG.md "$TAG"); then
  notes=$(gh release view "$TAG" --repo "$GITHUB_REPOSITORY" --json body --jq .body)
fi

assets='[]'
: > site/SHA256SUMS
for f in "$dir"/*; do
  name=$(basename "$f")
  sha=$(sha256sum "$f" | cut -d' ' -f1)
  url="$BASE_URL/download/$TAG/$name"
  assets=$(jq --arg name "$name" --arg url "$url" --arg sha "$sha" \
    '. + [{name: $name, url: $url, sha256: $sha}]' <<<"$assets")
  echo "$sha  download/$TAG/$name" >> site/SHA256SUMS
done

jq -n --arg version "${TAG#v}" --arg notes "$notes" --arg page "$BASE_URL/" \
  --argjson assets "$assets" \
  '{version: $version, notes: $notes, page_url: $page, assets: $assets}' \
  > site/latest.json

cp assets/icon.png site/icon.png
cp scripts/install.sh scripts/install.ps1 site/
python3 scripts/build-pages.py pages/index.html site/latest.json "$dir" site/index.html CHANGELOG.md

cat site/latest.json
