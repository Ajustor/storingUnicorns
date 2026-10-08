#!/usr/bin/env python3
"""Read CHANGELOG.md (Keep a Changelog layout: `## [X.Y.Z] - YYYY-MM-DD`).

Usage: changelog.py <CHANGELOG.md> <version>

Prints the body of that version's section (the release notes) and exits 1 when
the changelog has no such section. Also imported by build-pages.py.
"""
import re
import sys

HEADING = re.compile(r"^## \[([^\]]+)\](?:\s*-\s*(\S+))?\s*$")


def parse(path):
    """Released sections, newest first, as (version, date, markdown body)."""
    with open(path, encoding="utf-8") as f:
        lines = f.read().replace("\r\n", "\n").split("\n")
    sections, current = [], None
    for line in lines:
        m = HEADING.match(line)
        if m:
            current = (m.group(1), m.group(2) or "", [])
            sections.append(current)
        elif line.startswith("## "):
            current = None  # Any other level-2 heading ends the section.
        elif current is not None:
            current[2].append(line)
    return [
        (version, date, "\n".join(body).strip())
        for version, date, body in sections
        # An "Unreleased" section is not a version anyone can download.
        if re.match(r"^\d+\.\d+\.\d+", version)
    ]


def section(path, version):
    for v, _, body in parse(path):
        if v == version:
            return body
    return None


def main():
    path, version = sys.argv[1], sys.argv[2]
    if version.startswith("v"):
        version = version[1:]
    body = section(path, version)
    if body is None:
        print(f"No '## [{version}]' section in {path}", file=sys.stderr)
        sys.exit(1)
    # The notes are French and end up in JSON: UTF-8 and `\n` whatever the platform.
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
    print(body)


if __name__ == "__main__":
    main()
