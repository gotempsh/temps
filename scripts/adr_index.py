#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Generate and check docs/adr/README.md, the index of architecture decision records.

Rules enforced by --check (the default, run in CI):

* every ADR is docs/adr/NNN-slug.md, and no two ADRs share a number;
* its first heading is "# ADR-NNN: Title" (or "# ADR NNN: Title") with the
  number from its file name;
* every docs/adr/supporting/NNN-slug.md document belongs to an existing ADR NNN;
* every renumbered ADR listed in RENUMBERED still exists under its new number;
* docs/adr/README.md is exactly what --write generates from the files.

Usage:
    python3 scripts/adr_index.py            # check (same as --check)
    python3 scripts/adr_index.py --write    # regenerate docs/adr/README.md
    python3 scripts/adr_index.py --next     # print the next free ADR number
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ADR_DIR = Path("docs/adr")
SUPPORTING_DIR = "supporting"
INDEX_NAME = "README.md"

FILE_RE = re.compile(r"^(\d{3})-[a-z0-9]+(?:-[a-z0-9]+)*\.md$")
HEADING_RE = re.compile(r"^#\s+ADR[- ](\d{3})\s*:\s*(.+?)\s*$")
STATUS_LINE_RE = re.compile(r"^\s*(?:-\s+)?\*\*Status:?\*\*:?\s*(.+)$", re.IGNORECASE)
FRONTMATTER_STATUS_RE = re.compile(r"^status:\s*(.+)$", re.IGNORECASE)
# "**Supersedes:**", "**Supersedes the query design of:**", "**Superseded in part by:**",
# "**Amends:**", "**Amended by:**", optionally as a list item.
RELATION_LINE_RE = re.compile(
    r"^\s*(?:-\s+)?\*\*((Supersedes|Superseded|Amends|Amended)\b[^*]*)\*\*(.*)$",
    re.IGNORECASE,
)
LINK_TARGET_RE = re.compile(r"\]\((?:\./)?(\d{3})-[a-z0-9-]+\.md(?:#[^)]*)?\)")

# ADRs that shared a number with an older ADR and were moved to a free number
# on 2026-10-31. Older text (code comments, changelog entries, other ADRs) may
# still cite the old number, so the index keeps this table for disambiguation.
# Entries: (old number, new number). Never remove an entry.
RENUMBERED: tuple[tuple[str, str], ...] = (
    ("020", "071"),
    ("021", "072"),
    ("024", "073"),
    ("037", "074"),
    ("040", "075"),
    ("040", "076"),
    ("041", "077"),
    ("045", "078"),
    ("045", "079"),
    ("048", "080"),
    ("049", "081"),
    ("050", "082"),
    ("051", "083"),
    ("051", "084"),
    ("052", "085"),
    ("053", "086"),
    ("054", "087"),
    ("055", "088"),
)

# How each relation reads from the other side.
INVERSE = {
    "supersedes": "superseded by",
    "superseded by": "supersedes",
    "supersedes in part": "superseded in part by",
    "superseded in part by": "supersedes in part",
    "amends": "amended by",
    "amended by": "amends",
}
RELATION_ORDER = {kind: index for index, kind in enumerate(INVERSE)}


class AdrError(Exception):
    """An ADR file or the index violates the numbering rules."""


@dataclass
class Adr:
    number: str
    filename: str
    title: str
    status: str
    # (kind, target number) as declared in this ADR's own relation lines.
    relations: list[tuple[str, str]] = field(default_factory=list)


def short_status(raw: str) -> str:
    """The leading clause of a status line, e.g. "Accepted" from "Accepted — 2026-10-20"."""
    text = re.sub(r"[*`_]", "", raw).strip()
    text = re.split(r"\s+[—–]\s+|\s+-\s+|\s*\(|;|\.\s|\.$|,\s", text, maxsplit=1)[0].strip()
    if len(text) > 60:
        text = text[:57].rstrip() + "..."
    return text or "Unknown"


def parse_status(lines: list[str]) -> str:
    in_frontmatter = bool(lines) and lines[0].strip() == "---"
    for index, line in enumerate(lines):
        if in_frontmatter:
            if index > 0 and line.strip() == "---":
                in_frontmatter = False
                continue
            match = FRONTMATTER_STATUS_RE.match(line)
            if match:
                return short_status(match.group(1).strip().strip('"'))
            continue
        match = STATUS_LINE_RE.match(line)
        if match:
            return short_status(match.group(1))
        if line.strip().lower() == "## status":
            for following in lines[index + 1 :]:
                if following.strip():
                    return short_status(following)
    return "Unknown"


def relation_kind(label: str) -> str:
    """Map a bold relation label to a kind.

    "Supersedes:" is a full supersede; any qualifier ("Superseded in part by:",
    "Supersedes the query design of:") makes it partial. Amendments are partial
    by nature and keep their own kind.
    """
    words = re.sub(r"[^a-z ]", " ", label.lower()).split()
    keyword, rest = words[0], [w for w in words[1:] if w != "by"]
    if keyword == "amends":
        return "amends"
    if keyword == "amended":
        return "amended by"
    if keyword == "supersedes":
        return "supersedes in part" if rest else "supersedes"
    return "superseded in part by" if rest else "superseded by"


def parse_adr(path: Path) -> Adr:
    match = FILE_RE.match(path.name)
    if not match:
        raise AdrError(f"{path}: ADR file names must look like NNN-lowercase-slug.md")
    number = match.group(1)
    lines = path.read_text(encoding="utf-8").splitlines()
    heading = next((line for line in lines if line.startswith("# ")), None)
    if heading is None:
        raise AdrError(f"{path}: no '# ADR-{number}: Title' heading")
    heading_match = HEADING_RE.match(heading)
    if not heading_match:
        raise AdrError(f"{path}: first heading must be '# ADR-{number}: Title', found {heading!r}")
    if heading_match.group(1) != number:
        raise AdrError(
            f"{path}: heading says ADR {heading_match.group(1)} but the file name says {number}"
        )
    relations: list[tuple[str, str]] = []
    for line in lines:
        rel = RELATION_LINE_RE.match(line)
        if not rel:
            continue
        kind = relation_kind(rel.group(1))
        for target in LINK_TARGET_RE.findall(rel.group(3)):
            if target != number:
                relations.append((kind, target))
    return Adr(
        number=number,
        filename=path.name,
        title=heading_match.group(2),
        status=parse_status(lines),
        relations=relations,
    )


def collect(root: Path) -> tuple[list[Adr], list[tuple[str, str, str]]]:
    """Return the ADRs sorted by number, and supporting documents as (number, file, title)."""
    adr_dir = root / ADR_DIR
    adrs: list[Adr] = []
    by_number: dict[str, str] = {}
    errors: list[str] = []
    for path in sorted(adr_dir.glob("*.md")):
        if path.name == INDEX_NAME:
            continue
        try:
            adr = parse_adr(path)
        except AdrError as error:
            errors.append(str(error))
            continue
        if adr.number in by_number:
            errors.append(
                f"duplicate ADR number {adr.number}: {by_number[adr.number]} and {adr.filename}. "
                f"Give the newer one the next free number ({next_free_number(adr_dir)})."
            )
            continue
        by_number[adr.number] = adr.filename
        adrs.append(adr)
    for path in sorted(adr_dir.iterdir()):
        if path.is_file() and path.suffix != ".md":
            errors.append(f"{path}: only markdown files belong in {ADR_DIR}")

    supporting: list[tuple[str, str, str]] = []
    supporting_dir = adr_dir / SUPPORTING_DIR
    if supporting_dir.is_dir():
        for path in sorted(supporting_dir.glob("*.md")):
            match = FILE_RE.match(path.name)
            if not match:
                errors.append(f"{path}: supporting documents must be named NNN-slug.md after their ADR")
                continue
            if match.group(1) not in by_number:
                errors.append(f"{path}: supporting document for ADR {match.group(1)}, which does not exist")
                continue
            heading = next(
                (line[2:].strip() for line in path.read_text(encoding="utf-8").splitlines() if line.startswith("# ")),
                path.stem,
            )
            supporting.append((match.group(1), path.name, heading))

    for old, new in RENUMBERED:
        if new not in by_number:
            errors.append(f"RENUMBERED lists ADR {old} -> {new}, but no ADR {new} exists")

    if errors:
        raise AdrError("\n".join(errors))
    return adrs, supporting


def next_free_number(adr_dir: Path) -> str:
    numbers = [int(m.group(1)) for p in adr_dir.glob("*.md") if (m := FILE_RE.match(p.name))]
    return f"{(max(numbers) if numbers else 0) + 1:03d}"


def escape_cell(text: str) -> str:
    return text.replace("|", "\\|")


def render(adrs: list[Adr], supporting: list[tuple[str, str, str]]) -> str:
    by_number = {adr.number: adr for adr in adrs}
    links: dict[str, set[tuple[str, str]]] = {adr.number: set() for adr in adrs}
    for adr in adrs:
        for kind, target in adr.relations:
            if target not in by_number:
                continue
            links[adr.number].add((kind, target))
            links[target].add((INVERSE[kind], adr.number))

    def link(number: str) -> str:
        return f"[{number}](./{by_number[number].filename})"

    out = [
        "<!--",
        "SPDX-FileCopyrightText: 2024-2026 Temps Contributors",
        "SPDX-License-Identifier: MIT OR Apache-2.0",
        "-->",
        "",
        "# Architecture decision records",
        "",
        "<!-- Generated by scripts/adr_index.py. Do not edit by hand: run",
        "     `python3 scripts/adr_index.py --write` and commit the result. -->",
        "",
        "Every ADR has a unique three-digit number. To add one:",
        "",
        "1. Take the next free number from `python3 scripts/adr_index.py --next`.",
        "2. Name the file `docs/adr/NNN-short-slug.md` and start it with",
        "   `# ADR-NNN: Title` and a `**Status:**` line.",
        "3. Declare relations to other ADRs on their own lines, linking the file so the",
        "   number cannot be misread: `**Supersedes:** [ADR-NNN](./NNN-slug.md)`, or with a",
        "   qualifier for a partial one (`**Supersedes in part:**`, `**Superseded in part by:**`),",
        "   or `**Amends:**` / `**Amended by:**`. The index shows both directions.",
        "4. Run `python3 scripts/adr_index.py --write` and commit this file in the same PR.",
        "   CI runs `python3 scripts/adr_index.py --check`, which fails on a duplicate number",
        "   or a stale index.",
        "",
        "Companion documents (implementation plans, audits) live in `supporting/` under",
        "the number of the ADR they belong to; they do not take a number of their own.",
        "",
        "## Index",
        "",
        "| ADR | Title | Status | Related |",
        "|---|---|---|---|",
    ]
    for adr in adrs:
        related = "; ".join(
            f"{kind} {link(target)}"
            for kind, target in sorted(links[adr.number], key=lambda item: (RELATION_ORDER[item[0]], item[1]))
        )
        out.append(
            f"| {link(adr.number)} | {escape_cell(adr.title)} | {escape_cell(adr.status)} | {related} |"
        )

    if supporting:
        out += [
            "",
            "## Supporting documents",
            "",
            "| ADR | Document |",
            "|---|---|",
        ]
        for number, filename, title in supporting:
            out.append(f"| {link(number)} | [{escape_cell(title)}](./{SUPPORTING_DIR}/{filename}) |")

    out += [
        "",
        "## Renumbered ADRs",
        "",
        "Until 2026-10-31 several ADRs shared a number. The oldest file kept each number,",
        "except 055, which stayed with the workload plane ADR because almost every",
        "existing reference to ADR-055 means it. The others moved to new numbers.",
        "Text written before then (code comments, the changelog, other ADRs) may still",
        "use an old number: when a reference to one of these numbers does not match the",
        "ADR that holds it now, check this table.",
        "",
        "| Old number | Now | Title |",
        "|---|---|---|",
    ]
    for old, new in RENUMBERED:
        adr = by_number[new]
        out.append(f"| {old} | {link(new)} | {escape_cell(adr.title)} |")
    out.append("")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail if numbers collide or the index is stale (default)")
    mode.add_argument("--write", action="store_true", help="regenerate docs/adr/README.md")
    mode.add_argument("--next", action="store_true", help="print the next free ADR number")
    args = parser.parse_args(argv)

    adr_dir = args.root / ADR_DIR
    if args.next:
        print(next_free_number(adr_dir))
        return 0
    try:
        adrs, supporting = collect(args.root)
    except AdrError as error:
        print(f"ADR check failed:\n{error}", file=sys.stderr)
        return 1
    expected = render(adrs, supporting)
    index_path = adr_dir / INDEX_NAME
    if args.write:
        index_path.write_text(expected, encoding="utf-8")
        print(f"wrote {index_path.relative_to(args.root)} ({len(adrs)} ADRs)")
        return 0
    current = index_path.read_text(encoding="utf-8") if index_path.exists() else ""
    if current != expected:
        print(
            f"{index_path.relative_to(args.root)} is out of date. "
            "Run `python3 scripts/adr_index.py --write` and commit the result.",
            file=sys.stderr,
        )
        return 1
    print(f"ADR index OK: {len(adrs)} ADRs, numbers unique")
    return 0


if __name__ == "__main__":
    sys.exit(main())
