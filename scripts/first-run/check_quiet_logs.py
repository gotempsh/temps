#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Fail when an idle Temps server is not quiet.

Reads a `temps serve` log written with `TEMPS_LOG_FORMAT=full` (which puts
the module path on every line) and enforces two rules:

1. No ERROR line, and no Rust panic, anywhere in the log. An idle server
   with one app, one managed service, one alert rule and one monitor has
   nothing to be in error about; an ERROR an operator sees on day one
   teaches them to ignore ERRORs.
2. During the idle window, no single module logs more WARN lines per hour
   than `--warn-per-hour`. A WARN that fires every minute while nothing is
   happening is noise that buries the real ones.

Known, tracked exceptions live in an allowlist (TOML, see
quiet-logs-allowlist.toml). Every entry must link the issue tracking its
removal, so the list can only shrink by fixing things.

Exit status: 0 quiet, 1 noisy, 2 bad input.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
# `2026-10-04T10:00:00.123456Z  WARN span{a=1}: temps_proxy::proxy: message`
LINE = re.compile(
    r"^(?P<ts>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2}))\s+"
    r"(?P<level>TRACE|DEBUG|INFO|WARN|ERROR)\s+(?P<rest>.*)$"
)
# A module path followed by `: `. Span names are followed by `{` or are
# plain words; a target is the last `ident(::ident)*:` before the message.
TARGET = re.compile(r"(?:^|[\s:])(?P<target>[a-z_][a-z0-9_]*(?:::[a-z0-9_]+)*):\s")
PANIC = re.compile(r"thread '.*' panicked at|^\s*panicked at ")
ISSUE_URL = re.compile(r"^https://github\.com/[\w.-]+/[\w.-]+/issues/\d+$")
ALLOWED_KEYS = {"level", "target", "pattern", "issue", "reason"}


class InputError(Exception):
    """Raised for an unreadable log, malformed allowlist or bad argument."""


@dataclass
class LogLine:
    number: int
    timestamp: datetime | None
    level: str
    target: str
    text: str


@dataclass
class AllowEntry:
    level: str
    pattern: re.Pattern[str]
    issue: str
    reason: str
    target: str | None = None
    hits: int = 0

    def matches(self, line: LogLine) -> bool:
        if line.level != self.level:
            return False
        if self.target and not (line.target == self.target or line.target.startswith(self.target + "::")):
            return False
        return bool(self.pattern.search(line.text))


@dataclass
class Report:
    errors: list[LogLine] = field(default_factory=list)
    allowed_errors: list[LogLine] = field(default_factory=list)
    warn_counts: Counter[str] = field(default_factory=Counter)
    allowed_warn_counts: Counter[str] = field(default_factory=Counter)
    window_hours: float = 0.0
    threshold: float = 0.0
    lines_read: int = 0
    noisy_modules: dict[str, float] = field(default_factory=dict)
    stale_allow_entries: list[AllowEntry] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return not self.errors and not self.noisy_modules


def parse_timestamp(value: str) -> datetime:
    text = value.replace("Z", "+00:00")
    # Python < 3.11 accepts at most 6 fractional digits.
    text = re.sub(r"(\.\d{6})\d+", r"\1", text)
    parsed = datetime.fromisoformat(text)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def extract_target(rest: str) -> str:
    """Return the module path of a `full`-format line, or `unknown`."""
    candidates = [m.group("target") for m in TARGET.finditer(rest)]
    # Spans precede the target and carry `{fields}`; the target is the first
    # candidate that is a module path, falling back to the first word.
    for candidate in candidates:
        if "::" in candidate or candidate.startswith("temps"):
            return candidate
    return candidates[0] if candidates else "unknown"


def parse_log(lines: list[str]) -> list[LogLine]:
    parsed: list[LogLine] = []
    for number, raw in enumerate(lines, start=1):
        text = ANSI_ESCAPE.sub("", raw).rstrip("\n")
        match = LINE.match(text)
        if match:
            parsed.append(
                LogLine(
                    number=number,
                    timestamp=parse_timestamp(match.group("ts")),
                    level=match.group("level"),
                    target=extract_target(match.group("rest")),
                    text=text,
                )
            )
        elif PANIC.search(text) or re.search(r"\b(?:ERROR|FATAL)\b", text, re.IGNORECASE):
            parsed.append(LogLine(number=number, timestamp=None, level="ERROR", target="panic", text=text))
    if any(line.strip() for line in lines) and not parsed:
        raise InputError("nonempty server log contains no recognizable structured entries")
    return parsed


def load_allowlist(path: Path | None) -> list[AllowEntry]:
    if path is None:
        return []
    try:
        import tomllib
    except ModuleNotFoundError as error:  # pragma: no cover - Python < 3.11
        raise InputError("reading the allowlist needs Python 3.11+ (tomllib)") from error
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise InputError(f"cannot read allowlist {path}: {error}") from error
    entries: list[AllowEntry] = []
    for index, raw in enumerate(data.get("allow", []), start=1):
        where = f"{path} entry #{index}"
        unknown = set(raw) - ALLOWED_KEYS
        if unknown:
            raise InputError(f"{where}: unknown keys {sorted(unknown)}")
        level = str(raw.get("level", "")).upper()
        if level not in {"ERROR", "WARN"}:
            raise InputError(f"{where}: level must be ERROR or WARN, got {raw.get('level')!r}")
        issue = str(raw.get("issue", ""))
        if not ISSUE_URL.match(issue):
            raise InputError(f"{where}: issue must be a GitHub issue URL, got {issue!r}")
        reason = str(raw.get("reason", "")).strip()
        if not reason:
            raise InputError(f"{where}: reason is required")
        pattern = raw.get("pattern")
        if not pattern:
            raise InputError(f"{where}: pattern is required")
        try:
            compiled = re.compile(str(pattern))
        except re.error as error:
            raise InputError(f"{where}: invalid pattern {pattern!r}: {error}") from error
        entries.append(
            AllowEntry(level=level, pattern=compiled, issue=issue, reason=reason, target=raw.get("target"))
        )
    return entries


def allowed(line: LogLine, allowlist: list[AllowEntry]) -> bool:
    for entry in allowlist:
        if entry.matches(line):
            entry.hits += 1
            return True
    return False


def analyze(
    lines: list[LogLine],
    allowlist: list[AllowEntry],
    window_start: datetime | None,
    window_end: datetime | None,
    warn_per_hour: float,
) -> Report:
    report = Report(threshold=warn_per_hour, lines_read=len(lines))
    timestamps = [line.timestamp for line in lines if line.timestamp]
    start = window_start or (min(timestamps) if timestamps else None)
    end = window_end or (max(timestamps) if timestamps else None)
    if start and end:
        if window_start and window_end and end <= start:
            raise InputError(f"window end {end.isoformat()} is not after start {start.isoformat()}")
        report.window_hours = max(0.0, (end - start).total_seconds() / 3600)

    for line in lines:
        if line.level == "ERROR":
            (report.allowed_errors if allowed(line, allowlist) else report.errors).append(line)
        elif line.level == "WARN":
            in_window = line.timestamp is not None and (
                (start is None or line.timestamp >= start) and (end is None or line.timestamp <= end)
            )
            if not in_window:
                continue
            if allowed(line, allowlist):
                report.allowed_warn_counts[line.target] += 1
            else:
                report.warn_counts[line.target] += 1

    if report.window_hours > 0:
        for target, count in report.warn_counts.items():
            rate = count / report.window_hours
            if rate > warn_per_hour:
                report.noisy_modules[target] = rate
    report.stale_allow_entries = [entry for entry in allowlist if entry.hits == 0]
    return report


def normalize(text: str) -> str:
    """Collapse a log line to its shape so repeats group together."""
    body = LINE.sub(lambda m: f"{m.group('level')} {m.group('rest')}", text)
    body = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<uuid>", body)
    body = re.sub(r"\b[0-9a-f]{12,}\b", "<hex>", body)
    return re.sub(r"\d+", "<n>", body)


def render_markdown(report: Report, max_examples: int = 25) -> str:
    out = [f"## Quiet-logs soak: {'quiet' if report.ok else 'NOISY'}", ""]
    out.append(
        f"Read {report.lines_read} structured lines; idle window {report.window_hours * 60:.1f} min; "
        f"WARN threshold {report.threshold:g}/hour per module."
    )
    out += ["", f"### ERROR lines: {len(report.errors)} (+{len(report.allowed_errors)} allowlisted)", ""]
    if report.errors:
        grouped = Counter(normalize(line.text) for line in report.errors)
        first_seen: dict[str, LogLine] = {}
        for line in report.errors:
            first_seen.setdefault(normalize(line.text), line)
        out.append("| Count | First line | Module | Example |")
        out.append("| --- | --- | --- | --- |")
        for shape, count in grouped.most_common(max_examples):
            line = first_seen[shape]
            example = line.text.replace("|", "\\|")[:400]
            out.append(f"| {count} | {line.number} | `{line.target}` | `{example}` |")
    out += ["", "### WARN lines in the idle window, by module", ""]
    if report.warn_counts or report.allowed_warn_counts:
        out.append("| Module | WARN | Per hour | Allowlisted | Over threshold |")
        out.append("| --- | --- | --- | --- | --- |")
        modules = set(report.warn_counts) | set(report.allowed_warn_counts)
        for target in sorted(modules, key=lambda t: (-report.warn_counts[t], t)):
            count = report.warn_counts[target]
            rate = count / report.window_hours if report.window_hours else 0.0
            flag = "yes" if target in report.noisy_modules else ""
            out.append(f"| `{target}` | {count} | {rate:.1f} | {report.allowed_warn_counts[target]} | {flag} |")
    else:
        out.append("None.")
    if report.stale_allow_entries:
        out += ["", "### Allowlist entries that matched nothing (consider removing)", ""]
        for entry in report.stale_allow_entries:
            out.append(f"- {entry.level} `{entry.pattern.pattern}` ({entry.issue})")
    return "\n".join(out) + "\n"


def to_json(report: Report) -> dict[str, object]:
    return {
        "ok": report.ok,
        "window_hours": report.window_hours,
        "warn_per_hour_threshold": report.threshold,
        "errors": [{"line": l.number, "target": l.target, "text": l.text} for l in report.errors],
        "allowed_errors": len(report.allowed_errors),
        "warn_counts": dict(report.warn_counts),
        "allowed_warn_counts": dict(report.allowed_warn_counts),
        "noisy_modules": report.noisy_modules,
        "stale_allow_entries": [e.issue for e in report.stale_allow_entries],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("log", type=Path, help="temps serve log (TEMPS_LOG_FORMAT=full)")
    parser.add_argument("--allowlist", type=Path, help="TOML allowlist of tracked exceptions")
    parser.add_argument("--window-start", help="ISO-8601 start of the idle window (default: first line)")
    parser.add_argument("--window-end", help="ISO-8601 end of the idle window (default: last line)")
    parser.add_argument("--warn-per-hour", type=float, default=12.0, help="max WARN lines per hour per module")
    parser.add_argument("--markdown", type=Path, help="write a Markdown summary here")
    parser.add_argument("--json", type=Path, help="write a JSON report here")
    args = parser.parse_args(argv)

    try:
        try:
            raw_lines = args.log.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError as error:
            raise InputError(f"cannot read log {args.log}: {error}") from error
        if args.warn_per_hour < 0:
            raise InputError("--warn-per-hour must be >= 0")
        window_start = parse_timestamp(args.window_start) if args.window_start else None
        window_end = parse_timestamp(args.window_end) if args.window_end else None
        report = analyze(
            parse_log(raw_lines),
            load_allowlist(args.allowlist),
            window_start,
            window_end,
            args.warn_per_hour,
        )
    except (InputError, ValueError) as error:
        print(f"check_quiet_logs: {error}", file=sys.stderr)
        return 2

    markdown = render_markdown(report)
    print(markdown)
    if args.markdown:
        args.markdown.write_text(markdown, encoding="utf-8")
    if args.json:
        args.json.write_text(json.dumps(to_json(report), indent=2) + "\n", encoding="utf-8")
    return 0 if report.ok else 1


if __name__ == "__main__":
    sys.exit(main())
