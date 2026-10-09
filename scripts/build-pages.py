#!/usr/bin/env python3
"""Render the GitHub Pages download page from `pages/index.html`.

Usage: build-pages.py <template> <latest.json> <files-dir> <output> [CHANGELOG.md]

Fills `{{version}}`, `{{downloads}}` (one card per asset of the manifest),
`{{notes}}` (the release notes, a small Markdown subset rendered to HTML) and
`{{history}}` (the older versions of the changelog).
Run by the `pages` job of .github/workflows/release.yml.
"""
import html
import json
import os
import re
import sys

ICONS = {
    "windows": '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 5.5 10.5 4.5v7H3zM11.5 4.4 21 3v8.5h-9.5zM3 12.5h7.5v7L3 18.5zM11.5 12.5H21V21l-9.5-1.4z"/></svg>',
    "linux": '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2c-2.2 0-3.6 1.9-3.6 4.6 0 1.3.3 2.2-.4 3.6C6.8 12.5 5 14.6 5 17.2c0 .7.2 1.2.5 1.6-.8.4-1.5.9-1.5 1.6C4 21.6 6 22 7.6 22c1.3 0 2.2-.6 2.8-1.1.5.1 1 .1 1.6.1s1.1 0 1.6-.1c.6.5 1.5 1.1 2.8 1.1 1.6 0 3.6-.4 3.6-1.6 0-.7-.7-1.2-1.5-1.6.3-.4.5-.9.5-1.6 0-2.6-1.8-4.7-3-7-.7-1.4-.4-2.3-.4-3.6C15.6 3.9 14.2 2 12 2zm-1.4 4.2a.8 1 0 1 1 0 2 .8 1 0 0 1 0-2zm2.8 0a.8 1 0 1 1 0 2 .8 1 0 0 1 0-2zM12 9.3c.9 0 1.9.5 1.9.9S12.9 11 12 11s-1.9-.4-1.9-.8.9-.9 1.9-.9z"/></svg>',
    "macos": '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M16.4 12.6c0-2.4 2-3.5 2-3.6-1.1-1.6-2.8-1.8-3.4-1.8-1.4-.2-2.8.8-3.5.8s-1.8-.8-3-.8C7 7.3 5.5 8.2 4.7 9.7 3 12.6 4.3 17 5.9 19.3c.8 1.1 1.7 2.4 3 2.3 1.2 0 1.6-.8 3.1-.8s1.8.8 3.1.8c1.3 0 2.1-1.2 2.9-2.3.9-1.3 1.3-2.6 1.3-2.7 0 0-2.5-1-2.9-4zM14.1 5.6c.6-.8 1.1-1.9 1-3-.9 0-2.1.6-2.7 1.4-.6.7-1.1 1.8-1 2.9 1 .1 2.1-.5 2.7-1.3z"/></svg>',
    "autre": '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 2h8l6 6v14H6zm8 1.5V9h5.5z"/></svg>',
}

# (substring of the asset name, os, card title, detail) — first match wins.
KINDS = [
    (".msi", "windows", "Windows", "Installateur .msi — recommandé"),
    ("windows", "windows", "Windows", "Version portable .exe, sans installation"),
    ("linux-x64", "linux", "Linux", "Binaire x86_64"),
    ("linux", "linux", "Linux", "Binaire"),
    ("macos-arm64", "macos", "macOS", "Apple Silicon (M1 et suivants)"),
    ("macos-x64", "macos", "macOS", "Intel"),
    ("macos", "macos", "macOS", "Binaire"),
]


def classify(name):
    for needle, os_, title, detail in KINDS:
        if needle in name:
            return os_, title, detail
    return "autre", name, "Fichier"


def human_size(n):
    for unit in ("o", "Ko", "Mo", "Go"):
        if n < 1024 or unit == "Go":
            return f"{n:.0f} {unit}" if unit == "o" else f"{n:.1f} {unit}".replace(".", ",")
        n /= 1024


def card(asset, files_dir):
    name = asset["name"]
    os_, title, detail = classify(name)
    path = os.path.join(files_dir, name)
    size = f" · {human_size(os.path.getsize(path))}" if os.path.exists(path) else ""
    e = html.escape
    return f"""        <li class="carte" data-os="{os_}" data-label="{e(title)}">
          <span class="pastille">Votre système</span>
          <span class="os">{ICONS[os_]}{e(title)}</span>
          <span class="detail">{e(detail)}{size}</span>
          <a class="bouton principal" href="{e(asset['url'])}" download>Télécharger</a>
          <span class="fichier mono">{e(name)}</span>
          <details><summary>Empreinte SHA-256</summary><code>{e(asset['sha256'])}</code></details>
        </li>"""


# Windows first, MSI before the portable exe, then Linux, then macOS.
ORDER = {"windows": 0, "linux": 1, "macos": 2, "autre": 3}


def sort_key(asset):
    os_ = classify(asset["name"])[0]
    return (ORDER[os_], ".msi" not in asset["name"], asset["name"])


def inline(text):
    text = html.escape(text, quote=False)
    text = re.sub(r"`([^`]+)`", r"<code>\1</code>", text)
    text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"\[([^\]]+)\]\((https?://[^)\s]+)\)", r'<a href="\2">\1</a>', text)
    # Bare URLs (GitHub's generated notes end with a compare link).
    text = re.sub(r'(?<!href=")(?<!">)(https?://[^\s<]+)', r'<a href="\1">\1</a>', text)
    return text


def markdown(md, heading_base=3):
    """Headings, (nested) bullet lists and paragraphs: enough for release notes."""
    out, para = [], []
    depths = []  # Indentation of each open <ul>.

    def flush_para():
        if para:
            out.append(f"<p>{inline(' '.join(para))}</p>")
            para.clear()

    def close_lists(indent=-1):
        while depths and depths[-1] > indent:
            depths.pop()
            out.append("</li></ul>")

    for raw in md.replace("\r\n", "\n").split("\n"):
        line = raw.strip()
        indent = len(raw) - len(raw.lstrip())
        bullet = re.match(r"^[-*] (.*)", line)
        heading = re.match(r"^(#{1,6}) (.*)", line)
        if bullet:
            flush_para()
            close_lists(indent)
            if depths and depths[-1] == indent:
                out.append("</li>")
            else:
                out.append("<ul>")
                depths.append(indent)
            out.append(f"<li>{inline(bullet.group(1))}")
            continue
        if depths and line and indent > depths[-1]:
            # Continuation of the current list item.
            out[-1] += " " + inline(line)
            continue
        close_lists()
        if heading:
            flush_para()
            level = min(heading_base + len(heading.group(1)) - 3, 6)
            level = max(level, heading_base)
            out.append(f"<h{level}>{inline(heading.group(2))}</h{level}>")
        elif line:
            para.append(line)
        else:
            flush_para()
    close_lists()
    flush_para()
    return "\n".join(out)


def history(changelog_path, current):
    """Every released version but the current one, newest first, folded."""
    if not changelog_path or not os.path.exists(changelog_path):
        return ""
    from changelog import parse

    items = []
    for version, date, body in parse(changelog_path):
        if version == current:
            continue
        when = f' <span class="date">{html.escape(date)}</span>' if date else ""
        items.append(
            f"""        <details class="version">
          <summary><b>v{html.escape(version)}</b>{when}</summary>
          <div class="prose">
{markdown(body, heading_base=4)}
          </div>
        </details>"""
        )
    return "\n".join(items)


def main():
    template_path, manifest_path, files_dir, out_path = sys.argv[1:5]
    changelog_path = sys.argv[5] if len(sys.argv) > 5 else None
    with open(template_path, encoding="utf-8") as f:
        page = f.read()
    with open(manifest_path, encoding="utf-8") as f:
        manifest = json.load(f)

    assets = sorted(manifest["assets"], key=sort_key)
    page = (
        page.replace("{{version}}", html.escape(manifest["version"]))
        .replace("{{downloads}}", "\n".join(card(a, files_dir) for a in assets))
        .replace(
            "{{notes}}",
            markdown(manifest.get("notes") or "") or "<p>Pas de notes pour cette version.</p>",
        )
        .replace("{{history}}", history(changelog_path, manifest["version"]))
    )
    with open(out_path, "w", encoding="utf-8") as f:
        f.write(page)


if __name__ == "__main__":
    main()
