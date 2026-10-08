# Distribution — Implementation Plan (4/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tag-driven releases that build Windows/Linux/macOS binaries and an MSI, publish a GitHub Release, deploy a download page + `latest.json` + one-line install scripts to GitHub Pages, and publish to crates.io.

**Architecture:** Same pipeline as codingUnicorns (`Ajustor/codingUnicorns@2a994a7`): `release.yml` (verify → build matrix → release → pages → crates) and `pages.yml` (rebuild the site from the latest release when page sources change). `scripts/build-site.sh` assembles `site/` (binaries, `latest.json`, `SHA256SUMS`, `index.html`, install scripts). The page template and Python helpers are copied from codingUnicorns and adapted.

**Tech Stack:** GitHub Actions, WiX v3, bash + jq + python3 (site build), PowerShell (Windows install script), Pillow (icon drawing, local only).

Prerequisites: plans 1–3 on the branch. Conventions: `rtk` prefix, commit trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

Copying from codingUnicorns: the repo is private; fetch a file with
`gh api repos/Ajustor/codingUnicorns/contents/<path>?ref=2a994a7db18f46fda9289bffa42a193d2d3ba94f --jq .content | base64 -d > <dest>`.

Asset names used everywhere (must match `src/updater/mod.rs`):
`storingUnicorns-windows-x64.exe`, `storingUnicorns-setup.msi`, `storingUnicorns-linux-x64`, `storingUnicorns-macos-arm64`.

---

### Task 1: App icon

**Files:**
- Create: `scripts/draw-icon.py`, `scripts/New-Icon.ps1` (copied), `assets/icon.png`, `assets/icon.ico`, `build.rs`
- Modify: `Cargo.toml`, `src/gui/mod.rs`

- [ ] **Step 1: `scripts/draw-icon.py`**

```python
"""Draw the storingUnicorns icon: a database cylinder with a unicorn horn.

Rendered at 4x then downsampled for antialiasing. Requires Pillow:

    python scripts/draw-icon.py assets/icon.png 256

assets/icon.ico is derived from it by scripts/New-Icon.ps1.
"""
import sys
from PIL import Image, ImageDraw

S = 1024
BG_TOP, BG_BOTTOM = (139, 92, 246), (79, 40, 160)
DISK, DISK_EDGE = (245, 240, 255), (205, 190, 245)
HORN = [(255, 214, 102), (255, 160, 200), (180, 140, 255)]


def gradient(size, top, bottom):
    img = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / (size - 1)
        img.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    return img.resize((size, size))


def main():
    out, size = sys.argv[1], int(sys.argv[2])
    bg = gradient(S, BG_TOP, BG_BOTTOM).convert("RGBA")
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, S - 1, S - 1), radius=220, fill=255)
    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    icon.paste(bg, (0, 0), mask)
    d = ImageDraw.Draw(icon)

    # Database: three stacked disks.
    left, right, h = 262, 762, 120
    for i, top in enumerate((430, 560, 690)):
        d.rectangle((left, top + h // 2, right, top + h // 2 + 90), fill=DISK)
        d.ellipse((left, top + 90, right, top + 90 + h), fill=DISK, outline=DISK_EDGE, width=10)
        d.ellipse((left, top, right, top + h), fill=DISK, outline=DISK_EDGE, width=10)

    # Horn: striped triangle on the top disk.
    tip, base_l, base_r = (512, 120), (452, 470), (572, 470)
    d.polygon([tip, base_l, base_r], fill=HORN[0])
    for k, c in enumerate(HORN[1:], start=1):
        y = 470 - k * 110
        w = 60 * (y - tip[1]) / (470 - tip[1])
        d.line([(512 - w, y), (512 + w, y - 40)], fill=c, width=26)

    icon.resize((size, size), Image.LANCZOS).save(out)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Generate the PNG and the ICO**

```bash
pip install pillow
python scripts/draw-icon.py assets/icon.png 256
gh api "repos/Ajustor/codingUnicorns/contents/scripts/New-Icon.ps1?ref=2a994a7db18f46fda9289bffa42a193d2d3ba94f" --jq .content | base64 -d > scripts/New-Icon.ps1
pwsh -File scripts/New-Icon.ps1 -Source assets/icon.png -Destination assets/icon.ico
```

Expected: `assets/icon.png` (256×256, purple rounded square, white database, yellow horn) and `assets/icon.ico`. Open the PNG and check it reads well at small size; tweak coordinates if not.

- [ ] **Step 3: Embed the icon in the Windows exe** — `build.rs` at the repo root:

```rust
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // Gives the Windows executable its icon (Explorer, taskbar). Checked against
    // the target, not the host, so cross builds work.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not break the build.
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
```

`Cargo.toml`:

```toml
[build-dependencies]
winresource = "0.1"
```

- [ ] **Step 4: Window icon** — in `src/gui/mod.rs::run`, after building `viewport`:

```rust
let viewport = match eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon.png")) {
    Ok(icon) => viewport.with_icon(std::sync::Arc::new(icon)),
    Err(e) => {
        tracing::warn!("window icon: {e}");
        viewport
    }
};
```

- [ ] **Step 5: Verify**

Run: `cargo run` → the window and taskbar show the icon; on Windows the `.exe` in Explorer shows it.

- [ ] **Step 6: Commit**

```bash
rtk git add scripts/draw-icon.py scripts/New-Icon.ps1 assets build.rs Cargo.toml Cargo.lock src/gui/mod.rs
rtk git commit -m "feat: app icon"
```

---

### Task 2: CHANGELOG and version

**Files:**
- Create: `CHANGELOG.md`, `scripts/changelog.py` (copied)
- Modify: `Cargo.toml` (`version = "0.9.0"`)

- [ ] **Step 1: Copy the changelog parser**

```bash
gh api "repos/Ajustor/codingUnicorns/contents/scripts/changelog.py?ref=2a994a7db18f46fda9289bffa42a193d2d3ba94f" --jq .content | base64 -d > scripts/changelog.py
```

- [ ] **Step 2: `CHANGELOG.md`**

```markdown
# Journal des modifications

Les changements notables de storingUnicorns, version par version. Chaque section
est aussi affichée dans l'application lors d'une mise à jour et sur la
[page de téléchargement](https://ajustor.github.io/storingUnicorns/).

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et les
numéros de version suivent le [versionnage sémantique](https://semver.org/lang/fr/).

## [0.9.0] - 2026-10-08

### Nouveautés

- **Interface graphique** : connexions, arbre des tables, éditeur SQL avec onglets,
  coloration et autocomplétion, grille de résultats rapide même sur de gros volumes,
  édition des lignes, structure des tables, import/export CSV et SQL, vidage de tables.
- Exécution d'une requête annulable avec `Échap` ; l'interface ne se fige plus pendant une requête.
- **Mises à jour automatiques** : l'application propose les nouvelles versions et
  s'installe toute seule ; `storingUnicorns update` fait de même en ligne de commande.
- Installation en une ligne (`curl … | sh` ou `irm … | iex`) et installateur Windows `.msi`.
- Le TUI reste disponible : `storingUnicorns tui`.

### Changements

- `storingUnicorns` sans argument ouvre désormais l'interface graphique.
  `--debug` et `--no-animations` seuls continuent de lancer le TUI.
```

- [ ] **Step 3: Bump the version** in `Cargo.toml` to `0.9.0`.

- [ ] **Step 4: Verify the parser**

Run: `python scripts/changelog.py CHANGELOG.md v0.9.0`
Expected: prints the 0.9.0 body. `python scripts/changelog.py CHANGELOG.md v9.9.9; echo $?` → error message and `1`.

- [ ] **Step 5: Commit**

```bash
rtk git add CHANGELOG.md scripts/changelog.py Cargo.toml Cargo.lock
rtk git commit -m "chore: changelog and version 0.9.0"
```

---

### Task 3: Install scripts

**Files:**
- Create: `scripts/install.sh`, `scripts/install.ps1`

Both are served from the Pages root (Task 4 copies them into `site/`). `site/SHA256SUMS` lists `"<sha256>  download/<tag>/<asset>"` lines (Task 4).

- [ ] **Step 1: `scripts/install.sh`**

```sh
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
```

- [ ] **Step 2: `scripts/install.ps1`**

```powershell
# Install storingUnicorns on Windows (per user, no admin rights):
#
#   irm https://ajustor.github.io/storingUnicorns/install.ps1 | iex
#
# Installs into %LOCALAPPDATA%\Programs\storingUnicorns, adds it to the user PATH
# and creates a Start menu shortcut. The download is checked against the
# SHA-256 published in latest.json. The app then updates itself in place.
$ErrorActionPreference = 'Stop'

$base = 'https://ajustor.github.io/storingUnicorns'
$assetName = 'storingUnicorns-windows-x64.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\storingUnicorns'
$exe = Join-Path $dir 'storingUnicorns.exe'

$manifest = Invoke-RestMethod "$base/latest.json"
$asset = $manifest.assets | Where-Object name -eq $assetName
if (-not $asset) { throw "Aucun binaire $assetName dans la version $($manifest.version)." }

New-Item -ItemType Directory -Force -Path $dir | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) $assetName
Write-Host "Téléchargement de storingUnicorns v$($manifest.version)…"
Invoke-WebRequest $asset.url -OutFile $tmp -UseBasicParsing

$actual = (Get-FileHash $tmp -Algorithm SHA256).Hash
if ($actual -ne $asset.sha256.ToUpper()) {
    Remove-Item $tmp -Force
    throw "Empreinte SHA-256 incorrecte (attendu $($asset.sha256), obtenu $actual)."
}
Move-Item $tmp $exe -Force

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ";$dir"), 'User')
    Write-Host "$dir ajouté au PATH (ouvrez un nouveau terminal)."
}

$startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\storingUnicorns.lnk'
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($startMenu)
$link.TargetPath = $exe
$link.WorkingDirectory = $dir
$link.Save()

Write-Host "storingUnicorns v$($manifest.version) installé. Lancez-le depuis le menu Démarrer ou avec : storingUnicorns"
```

- [ ] **Step 3: Lint**

Run: `bash -n scripts/install.sh` (and `shellcheck scripts/install.sh` if available) → no errors.
Run: `pwsh -NoProfile -Command "[ScriptBlock]::Create((Get-Content -Raw scripts/install.ps1)) | Out-Null"` → no parse error.

- [ ] **Step 4: Commit**

```bash
rtk git add scripts/install.sh scripts/install.ps1
rtk git commit -m "feat: one-line install scripts"
```

---

### Task 4: Site build and download page

**Files:**
- Create: `scripts/build-site.sh`, `scripts/build-pages.py` (copied + adapted), `pages/index.html` (copied head + new body)

- [ ] **Step 1: Copy `build-pages.py`**

```bash
gh api "repos/Ajustor/codingUnicorns/contents/scripts/build-pages.py?ref=2a994a7db18f46fda9289bffa42a193d2d3ba94f" --jq .content | base64 -d > scripts/build-pages.py
```

No edits needed: its `KINDS` table classifies assets by substring (`.msi`, `windows`, `linux-x64`, `macos-arm64`), which already matches our four asset names. Check that `KINDS` in the copied file still contains those four needles.

- [ ] **Step 2: `scripts/build-site.sh`**

```bash
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
```

- [ ] **Step 3: `pages/index.html`** — copy the codingUnicorns template, keep everything up to and including `</head>`, and replace product strings in the head:

```bash
gh api "repos/Ajustor/codingUnicorns/contents/pages/index.html?ref=2a994a7db18f46fda9289bffa42a193d2d3ba94f" --jq .content | base64 -d > pages/index.html
```

In the `<head>`:
- `<title>storingUnicorns {{version}}</title>`
- description: `storingUnicorns — un client de bases de données léger, en Rust : interface graphique rapide et interface terminal. PostgreSQL, MySQL, SQLite, SQL Server, Azure SQL.`
- `og:title` → `storingUnicorns`; `og:description` → `Un client de bases de données léger, en Rust.`

Add these rules just before `</style>`:

```css
    pre.commande {
      margin: 0.5rem 0 1rem;
      padding: 0.75rem 1rem;
      border-radius: 10px;
      background: var(--marque-douce);
      overflow-x: auto;
      font-size: 0.95rem;
    }
    [data-theme="sombre"] pre.commande { background: rgba(139, 92, 246, 0.15); }
    @media (prefers-color-scheme: dark) {
      :root:not([data-theme="clair"]) pre.commande { background: rgba(139, 92, 246, 0.15); }
    }
```

Replace everything from `<body>` to the end of the file with:

```html
<body>
<div class="page">
  <a class="saut" href="#contenu">Aller au contenu</a>

  <header class="borne">
    <a class="marque" href="./">
      <img src="./icon.png" alt="" width="28" height="28">
      <span>storingUnicorns</span>
    </a>
    <nav aria-label="Navigation principale">
      <a class="secondaire-nav" href="#fonctionnalites">Fonctionnalités</a>
      <a class="secondaire-nav" href="#installer">Installer</a>
      <a href="#telecharger">Télécharger</a>
      <button type="button" class="theme" id="theme" aria-label="Changer de thème" title="Changer de thème"><span class="vers-clair" aria-hidden="true">☀</span><span class="vers-sombre" aria-hidden="true">☾</span></button>
    </nav>
  </header>

  <main id="contenu" class="borne">
    <section class="hero">
      <p class="etat mono">
        <span class="point" aria-hidden="true"></span>
        v{{version}} · dernière version stable
      </p>

      <div class="logo"><img src="./icon.png" alt="" width="112" height="112"></div>

      <h1>storing<span class="suffixe">Unicorns</span></h1>

      <p class="accroche">Un client de bases de données léger, construit en Rust.</p>

      <p class="lead">
        Une interface graphique rapide pour parcourir vos tables, écrire du SQL et
        modifier vos données — et la même chose dans le terminal, pour ceux qui n'en
        sortent jamais.
      </p>

      <div class="actions">
        <a class="bouton principal" id="cta" href="#telecharger">Télécharger v{{version}}</a>
        <a class="bouton secondaire" href="#installer">Installer en une ligne</a>
      </div>
      <p class="petit">Windows · Linux · macOS (Apple Silicon) — mises à jour automatiques</p>
    </section>

    <section class="chiffres" aria-labelledby="chiffres-titre">
      <h2 id="chiffres-titre" class="surtitre">En quelques chiffres</h2>
      <ul>
        <li><b>5</b><span>moteurs : PostgreSQL, MySQL, SQLite, SQL Server, Azure SQL</span></li>
        <li><b>2</b><span>interfaces : graphique et terminal, dans un seul binaire</span></li>
        <li><b>1</b><span>fichier à télécharger, qui se met à jour tout seul</span></li>
        <li><b>3</b><span>plateformes : Windows, Linux et macOS</span></li>
      </ul>
    </section>

    <section class="bloc" id="installer" aria-labelledby="installer-titre">
      <h2 id="installer-titre">Installer en une ligne</h2>
      <p class="intro">Le script télécharge le binaire de votre système, vérifie son empreinte SHA-256 et l'ajoute à votre <code>PATH</code>.</p>
      <h3>Linux / macOS</h3>
      <pre class="commande mono">curl -fsSL https://ajustor.github.io/storingUnicorns/install.sh | sh</pre>
      <h3>Windows (PowerShell)</h3>
      <pre class="commande mono">irm https://ajustor.github.io/storingUnicorns/install.ps1 | iex</pre>
      <p class="astuce">
        Ensuite : <code>storingUnicorns</code> ouvre l'interface graphique,
        <code>storingUnicorns tui</code> l'interface terminal,
        <code>storingUnicorns update</code> met à jour.
      </p>
    </section>

    <section class="bloc" id="telecharger" aria-labelledby="telecharger-titre">
      <h2 id="telecharger-titre">Télécharger la v{{version}}</h2>
      <p class="intro">
        Choisissez le fichier de votre système. Une fois installé, storingUnicorns se
        met à jour tout seul : il vérifie au démarrage si une nouvelle version existe
        et contrôle chaque téléchargement par son empreinte SHA-256.
      </p>
      <ul class="telechargements">
{{downloads}}
      </ul>
      <p class="astuce">
        <strong>Linux / macOS :</strong> rendez le binaire exécutable avec
        <code>chmod +x storingUnicorns-*</code> puis lancez-le.
        <strong>Windows :</strong> l'installateur <code>.msi</code> installe pour tous les
        utilisateurs ; le <code>.exe</code> est une version portable, sans installation.
      </p>
    </section>

    <section class="bloc" id="fonctionnalites" aria-labelledby="fonctionnalites-titre">
      <h2 id="fonctionnalites-titre">Tout ce qu'il faut, rien de plus</h2>
      <ul class="piliers">
        <li>
          <h3>🔌 Connexions</h3>
          <ul>
            <li>PostgreSQL, MySQL, SQLite, SQL Server</li>
            <li>Azure SQL : identifiants, Azure AD ou identité managée</li>
            <li>Test de connexion avant enregistrement</li>
          </ul>
        </li>
        <li>
          <h3>✏️ Éditeur SQL</h3>
          <ul>
            <li>Onglets conservés entre les sessions</li>
            <li>Coloration et autocomplétion des tables et colonnes</li>
            <li>Instruction ou transaction au curseur <kbd>Ctrl+Entrée</kbd></li>
            <li>Annulation d'une requête longue <kbd>Échap</kbd></li>
          </ul>
        </li>
        <li>
          <h3>📊 Résultats</h3>
          <ul>
            <li>Grille fluide même sur des centaines de milliers de lignes</li>
            <li>Filtre instantané, copie de cellules et de lignes</li>
            <li>Modifier, ajouter et supprimer des lignes</li>
          </ul>
        </li>
        <li>
          <h3>🧱 Structure</h3>
          <ul>
            <li>Colonnes, types, clés primaires</li>
            <li>Ajouter, renommer, modifier, supprimer une colonne</li>
          </ul>
        </li>
        <li>
          <h3>📦 Import / export</h3>
          <ul>
            <li>Export CSV ou SQL INSERT</li>
            <li>Import CSV avec mise à jour par identifiant</li>
            <li>Export, import et vidage de plusieurs tables d'un coup</li>
          </ul>
        </li>
        <li>
          <h3>🖥️ Terminal</h3>
          <ul>
            <li>Toute l'application au clavier : <code>storingUnicorns tui</code></li>
            <li>Mêmes connexions et mêmes onglets que l'interface graphique</li>
          </ul>
        </li>
      </ul>
    </section>

    <section class="bloc" id="nouveautes" aria-labelledby="nouveautes-titre">
      <h2 id="nouveautes-titre">Nouveautés de la v{{version}}</h2>
      <div class="prose">
{{notes}}
      </div>
      <div class="historique">
        <h3 class="surtitre">Historique des versions</h3>
{{history}}
      </div>
    </section>
  </main>

  <footer class="borne">
    <p>🦄 storingUnicorns v{{version}} · Licence MIT · Construit en Rust avec egui et ratatui</p>
    <span><a href="#nouveautes">Nouveautés</a> · <a href="https://github.com/Ajustor/storingUnicorns">Code source</a> · <a href="./latest.json">latest.json</a></span>
  </footer>
</div>
```

followed by the original `<script>` block (theme toggle + OS detection) and `</body></html>`, copied unchanged from the template.

- [ ] **Step 4: Build the site locally with fake artifacts**

```bash
mkdir -p /tmp/su-art && for n in storingUnicorns-windows-x64.exe storingUnicorns-setup.msi storingUnicorns-linux-x64 storingUnicorns-macos-arm64; do echo "$n" > /tmp/su-art/$n; done
bash scripts/build-site.sh v0.9.0 https://ajustor.github.io/storingUnicorns /tmp/su-art
```

(On Windows, run it from Git Bash; `jq` must be installed.)
Expected: `site/` contains `index.html`, `latest.json` (4 assets, notes = the 0.9.0 changelog section), `SHA256SUMS` (4 lines), `install.sh`, `install.ps1`, `icon.png`, `download/v0.9.0/*`. Open `site/index.html` in a browser: hero, install commands, 4 download cards with your OS highlighted, notes rendered, light/dark toggle works, no horizontal scroll at phone width. Then `rm -rf site` (add `site/` to `.gitignore`).

- [ ] **Step 5: Commit**

```bash
rtk git add scripts/build-site.sh scripts/build-pages.py pages/index.html .gitignore
rtk git commit -m "feat: GitHub Pages download page and site build"
```

---

### Task 5: Windows installer (WiX)

**Files:**
- Create: `wix/main.wxs`, `wix/License.rtf` (copied)

- [ ] **Step 1: Copy the blank licence**

```bash
gh api "repos/Ajustor/codingUnicorns/contents/wix/License.rtf?ref=2a994a7db18f46fda9289bffa42a193d2d3ba94f" --jq .content | base64 -d > wix/License.rtf
```

- [ ] **Step 2: `wix/main.wxs`** (new UpgradeCode; never change it after the first release)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!--
  WiX v3 installer for storingUnicorns.

  Build (from the project root, after `cargo build --release`):
    candle.exe -arch x64 -dProductVersion=0.9.0 -out wix\main.wixobj wix\main.wxs
    light.exe  -ext WixUIExtension -out storingUnicorns-setup.msi wix\main.wixobj
-->
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id="*"
           Name="storingUnicorns"
           Language="1036"
           Version="$(var.ProductVersion)"
           Manufacturer="Ajustor"
           UpgradeCode="{6F1D2C8A-3B7E-4E52-9A41-0C5B8D7E2F19}">

    <Package InstallerVersion="500" Compressed="yes" InstallScope="perMachine"
             Description="storingUnicorns — client de bases de données" />

    <MajorUpgrade DowngradeErrorMessage="Une version plus récente de storingUnicorns est déjà installée." />
    <MediaTemplate EmbedCab="yes" />

    <Icon Id="AppIcon" SourceFile="assets\icon.ico" />
    <Property Id="ARPPRODUCTICON" Value="AppIcon" />
    <Property Id="ARPHELPLINK" Value="https://ajustor.github.io/storingUnicorns/" />

    <UIRef Id="WixUI_Minimal" />
    <WixVariable Id="WixUILicenseRtf" Value="wix\License.rtf" />

    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="ProgramFiles64Folder">
        <Directory Id="INSTALLFOLDER" Name="storingUnicorns" />
      </Directory>
      <Directory Id="ProgramMenuFolder" />
    </Directory>

    <Feature Id="ProductFeature" Title="storingUnicorns" Level="1">
      <ComponentRef Id="MainExecutable" />
      <ComponentRef Id="StartMenuShortcut" />
    </Feature>

    <DirectoryRef Id="INSTALLFOLDER">
      <Component Id="MainExecutable" Guid="*">
        <File Id="MainExecutable" Name="storingUnicorns.exe"
              Source="target\release\storingUnicorns.exe" KeyPath="yes" />
        <!-- `storingUnicorns tui` / `update` from any terminal -->
        <Environment Id="PATH" Name="PATH" Value="[INSTALLFOLDER]" Permanent="no"
                     Part="last" Action="set" System="yes" />
      </Component>
    </DirectoryRef>

    <DirectoryRef Id="ProgramMenuFolder">
      <Component Id="StartMenuShortcut" Guid="*">
        <Shortcut Id="StartMenuLink" Name="storingUnicorns"
                  Description="Client de bases de données"
                  Target="[INSTALLFOLDER]storingUnicorns.exe"
                  WorkingDirectory="INSTALLFOLDER" Icon="AppIcon" />
        <RegistryValue Root="HKCU" Key="Software\storingUnicorns" Name="StartMenu"
                       Type="integer" Value="1" KeyPath="yes" />
      </Component>
    </DirectoryRef>
  </Product>
</Wix>
```

- [ ] **Step 3: Build it locally if WiX v3 is installed** (otherwise CI validates it in Task 6)

```powershell
cargo build --release
& "${env:WIX}bin\candle.exe" -arch x64 -dProductVersion=0.9.0 -out wix\main.wixobj wix\main.wxs
& "${env:WIX}bin\light.exe" -ext WixUIExtension -out storingUnicorns-setup.msi wix\main.wixobj
```

Expected: `storingUnicorns-setup.msi`. Install it: Start menu entry with icon, `storingUnicorns --version` works in a new terminal, launching from the Start menu shows no console window. Uninstall removes both. Delete the `.msi` and `wix\main.wixobj` afterwards (add `*.wixobj` and `*.msi` to `.gitignore`).

- [ ] **Step 4: Commit**

```bash
rtk git add wix .gitignore
rtk git commit -m "feat: Windows MSI installer"
```

---

### Task 6: Release and Pages workflows

**Files:**
- Replace: `.github/workflows/release.yml`
- Create: `.github/workflows/pages.yml`

- [ ] **Step 1: `.github/workflows/release.yml`**

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

env:
  CARGO_TERM_COLOR: always

permissions:
  contents: write

jobs:
  verify-version:
    name: Verify tag matches Cargo.toml
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5

      # The in-app updater compares the release tag with CARGO_PKG_VERSION; a
      # mismatch would make every client re-offer the update forever.
      - name: Compare versions
        run: |
          cargo_version=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
          tag_version="${GITHUB_REF_NAME#v}"
          if [ "$cargo_version" != "$tag_version" ]; then
            echo "::error::Tag $GITHUB_REF_NAME does not match Cargo.toml version $cargo_version"
            exit 1
          fi

      - name: Check CHANGELOG.md entry
        if: ${{ !contains(github.ref_name, '-') }}
        run: |
          if ! python3 scripts/changelog.py CHANGELOG.md "$GITHUB_REF_NAME" > /dev/null; then
            echo "::error::CHANGELOG.md has no '## [${GITHUB_REF_NAME#v}]' section"
            exit 1
          fi

  build:
    name: Build ${{ matrix.artifact }}
    needs: verify-version
    runs-on: ${{ matrix.os }}
    env:
      # Authenticode signing is opt-in: it runs only when both secrets are set.
      HAS_CERT: ${{ secrets.WINDOWS_CERT_PFX_BASE64 != '' && secrets.WINDOWS_CERT_PASSWORD != '' }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            artifact: storingUnicorns-linux-x64
          - os: macos-latest
            target: aarch64-apple-darwin
            artifact: storingUnicorns-macos-arm64
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            artifact: storingUnicorns-windows-x64.exe
    steps:
      - uses: actions/checkout@v5

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - uses: Swatinem/rust-cache@v2
        with:
          key: release-${{ matrix.target }}

      - name: Install Linux system dependencies
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libssl-dev libxcb-render0-dev \
            libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libglib2.0-dev

      - name: Test
        run: cargo test --target ${{ matrix.target }}

      - name: Build release binary
        run: cargo build --release --target ${{ matrix.target }}

      - name: Prepare code signing (Windows)
        if: matrix.os == 'windows-latest' && env.HAS_CERT == 'true'
        shell: pwsh
        env:
          WINDOWS_CERT_PFX_BASE64: ${{ secrets.WINDOWS_CERT_PFX_BASE64 }}
        run: |
          $pfx = Join-Path $env:RUNNER_TEMP 'codesign.pfx'
          [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64))
          $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
                      Sort-Object FullName -Descending | Select-Object -First 1
          if (-not $signtool) { throw 'signtool.exe not found' }
          "CODESIGN_PFX=$pfx" >> $env:GITHUB_ENV
          "SIGNTOOL=$($signtool.FullName)" >> $env:GITHUB_ENV

      - name: Sign binary (Windows)
        if: matrix.os == 'windows-latest' && env.HAS_CERT == 'true'
        shell: pwsh
        env:
          WINDOWS_CERT_PASSWORD: ${{ secrets.WINDOWS_CERT_PASSWORD }}
        run: |
          $exe = "target\${{ matrix.target }}\release\storingUnicorns.exe"
          & $env:SIGNTOOL sign /f $env:CODESIGN_PFX /p $env:WINDOWS_CERT_PASSWORD `
            /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /d "storingUnicorns" $exe
          if ($LASTEXITCODE -ne 0) { throw "signtool sign failed" }

      - name: Rename binary (Unix)
        if: matrix.os != 'windows-latest'
        run: cp target/${{ matrix.target }}/release/storingUnicorns ${{ matrix.artifact }}

      - name: Rename binary (Windows)
        if: matrix.os == 'windows-latest'
        run: cp target/${{ matrix.target }}/release/storingUnicorns.exe ${{ matrix.artifact }}

      - name: Build Windows installer (MSI)
        if: matrix.os == 'windows-latest'
        shell: pwsh
        run: |
          New-Item -ItemType Directory -Force -Path target\release | Out-Null
          Copy-Item target\${{ matrix.target }}\release\storingUnicorns.exe target\release\storingUnicorns.exe -Force
          $wixDir = Get-ChildItem "C:\Program Files (x86)\WiX Toolset*" | Sort-Object Name -Descending | Select-Object -First 1
          if (-not $wixDir) {
            choco install wixtoolset --no-progress -y
            $wixDir = Get-ChildItem "C:\Program Files (x86)\WiX Toolset*" | Sort-Object Name -Descending | Select-Object -First 1
          }
          $candle = Join-Path $wixDir.FullName "bin\candle.exe"
          $light  = Join-Path $wixDir.FullName "bin\light.exe"
          # MSI versions are x.y.z only.
          $version = "${{ github.ref_name }}".TrimStart('v').Split('-')[0]
          & $candle -arch x64 "-dProductVersion=$version" -out wix\main.wixobj wix\main.wxs
          if ($LASTEXITCODE -ne 0) { throw "candle failed" }
          & $light -ext WixUIExtension -out storingUnicorns-setup.msi wix\main.wixobj
          if ($LASTEXITCODE -ne 0) { throw "light failed" }

      - name: Sign installer (Windows)
        if: matrix.os == 'windows-latest' && env.HAS_CERT == 'true'
        shell: pwsh
        env:
          WINDOWS_CERT_PASSWORD: ${{ secrets.WINDOWS_CERT_PASSWORD }}
        run: |
          & $env:SIGNTOOL sign /f $env:CODESIGN_PFX /p $env:WINDOWS_CERT_PASSWORD `
            /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /d "storingUnicorns Installer" storingUnicorns-setup.msi
          if ($LASTEXITCODE -ne 0) { throw "signtool sign failed" }

      - name: Remove signing certificate (Windows)
        if: always() && matrix.os == 'windows-latest' && env.HAS_CERT == 'true'
        shell: pwsh
        run: if ($env:CODESIGN_PFX -and (Test-Path $env:CODESIGN_PFX)) { Remove-Item $env:CODESIGN_PFX -Force }

      - uses: actions/upload-artifact@v5
        with:
          name: ${{ matrix.artifact }}
          path: ${{ matrix.artifact }}

      - if: matrix.os == 'windows-latest'
        uses: actions/upload-artifact@v5
        with:
          name: storingUnicorns-setup-msi
          path: storingUnicorns-setup.msi

  release:
    name: Create GitHub Release
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/download-artifact@v5
        with:
          path: artifacts/
      - name: Release notes
        id: notes
        run: |
          if python3 scripts/changelog.py CHANGELOG.md "$GITHUB_REF_NAME" > release-notes.md; then
            echo "generate=false" >> "$GITHUB_OUTPUT"
          else
            : > release-notes.md
            echo "generate=true" >> "$GITHUB_OUTPUT"
          fi
      - uses: softprops/action-gh-release@v2
        with:
          name: storingUnicorns ${{ github.ref_name }}
          prerelease: ${{ contains(github.ref_name, '-') }}
          body_path: release-notes.md
          files: artifacts/**/*
          generate_release_notes: ${{ steps.notes.outputs.generate }}

  # Binaries + latest.json + install scripts on GitHub Pages: what the in-app
  # updater (src/updater/mod.rs) and scripts/install.* read. Each deploy
  # replaces the whole site, so only the latest stable release is hosted.
  pages:
    name: Publish to GitHub Pages
    needs: release
    if: ${{ !contains(github.ref_name, '-') }}
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pages: write
      id-token: write
    concurrency:
      group: pages
      cancel-in-progress: false
    environment:
      name: github-pages
      url: ${{ steps.deploy.outputs.page_url }}
    steps:
      - uses: actions/checkout@v5
      - uses: actions/download-artifact@v5
        with:
          path: artifacts/
      - id: pages
        uses: actions/configure-pages@v5
      - name: Build site
        env:
          GH_TOKEN: ${{ github.token }}
          BASE_URL: ${{ steps.pages.outputs.base_url }}
        run: bash scripts/build-site.sh "$GITHUB_REF_NAME" "$BASE_URL" artifacts
      - uses: actions/upload-pages-artifact@v4
        with:
          path: site
      - id: deploy
        uses: actions/deploy-pages@v4

  crates:
    name: Publish to crates.io
    needs: release
    if: ${{ !contains(github.ref_name, '-') }}
    runs-on: ubuntu-latest
    env:
      HAS_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN != '' }}
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - name: Install Linux system dependencies
        if: env.HAS_TOKEN == 'true'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libssl-dev libxcb-render0-dev \
            libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libglib2.0-dev
      - name: cargo publish
        if: env.HAS_TOKEN == 'true'
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
        run: cargo publish
```

- [ ] **Step 2: `.github/workflows/pages.yml`**

```yaml
name: Pages

# Rebuild the download page when its sources change on master, without waiting
# for the next release. The site is rebuilt from the latest stable release's
# assets (each deploy replaces the whole site).
on:
  push:
    branches: [master]
    paths:
      - 'pages/**'
      - 'CHANGELOG.md'
      - 'scripts/changelog.py'
      - 'scripts/build-pages.py'
      - 'scripts/build-site.sh'
      - 'scripts/install.sh'
      - 'scripts/install.ps1'
      - 'assets/icon.png'
      - '.github/workflows/pages.yml'
  workflow_dispatch:

permissions:
  contents: read
  pages: write
  id-token: write

concurrency:
  group: pages
  cancel-in-progress: false

jobs:
  pages:
    runs-on: ubuntu-latest
    environment:
      name: github-pages
      url: ${{ steps.deploy.outputs.page_url }}
    steps:
      - uses: actions/checkout@v5
      - name: Find latest stable release
        id: release
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          tag=$(gh release list --repo "$GITHUB_REPOSITORY" --exclude-drafts --exclude-pre-releases \
            --limit 1 --json tagName --jq '.[0].tagName // empty')
          if [ -z "$tag" ]; then
            echo "::error::No stable release to publish"
            exit 1
          fi
          echo "tag=$tag" >> "$GITHUB_OUTPUT"
      - name: Download release assets
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh release download "${{ steps.release.outputs.tag }}" --repo "$GITHUB_REPOSITORY" --dir artifacts
      - id: pages
        uses: actions/configure-pages@v5
      - name: Build site
        env:
          GH_TOKEN: ${{ github.token }}
          BASE_URL: ${{ steps.pages.outputs.base_url }}
        run: bash scripts/build-site.sh "${{ steps.release.outputs.tag }}" "$BASE_URL" artifacts
      - uses: actions/upload-pages-artifact@v4
        with:
          path: site
      - id: deploy
        uses: actions/deploy-pages@v4
```

Note: old releases (≤ v0.8.x) have assets with other names. `pages.yml` only works once v0.9.0 is released; until then it fails with "No stable release" or publishes old assets the updater ignores (harmless).

- [ ] **Step 3: Validate the YAML**

Run: `python -c "import yaml,sys; [yaml.safe_load(open(f)) for f in sys.argv[1:]]" .github/workflows/release.yml .github/workflows/pages.yml` (or `actionlint` if installed).
Expected: no error.

- [ ] **Step 4: README — install and release**

Append to `README.md`:

```markdown
## Installer

- Linux / macOS : `curl -fsSL https://ajustor.github.io/storingUnicorns/install.sh | sh`
- Windows : `irm https://ajustor.github.io/storingUnicorns/install.ps1 | iex`, ou l'installateur `.msi`
  sur la [page de téléchargement](https://ajustor.github.io/storingUnicorns/)
- Depuis les sources : `cargo install storingUnicorns`

## Publier une version

1. Mettre à jour `version` dans `Cargo.toml` et ajouter la section `## [X.Y.Z] - AAAA-MM-JJ` dans `CHANGELOG.md`.
2. Commit, puis `git tag vX.Y.Z && git push origin master vX.Y.Z`.
3. Le workflow Release construit les binaires et le MSI, crée la release GitHub,
   publie la page de téléchargement + `latest.json` sur GitHub Pages, puis publie sur crates.io.
```

- [ ] **Step 5: Commit**

```bash
rtk git add .github/workflows README.md
rtk git commit -m "ci: tag-driven release with MSI, GitHub Pages and crates.io"
```

- [ ] **Step 6: Hand-off to the user (do not do these yourself)**

Tell the user:
1. In GitHub → Settings → Pages, set **Source = GitHub Actions** (currently "Deploy from a branch, master /docs").
2. Merge the branch, then release with `git tag v0.9.0 && git push origin v0.9.0`.
3. After the workflow: open https://ajustor.github.io/storingUnicorns/, try both install one-liners, and check `storingUnicorns update` says "up to date".
