#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Count CLAUDE.md rule violations per crate and fail when a count rises.

The rules in CLAUDE.md that clippy cannot express are counted here and
compared with the committed baseline in ``scripts/rule-baseline.json``. The
gate only fails when a count goes *up*, so it can land on a tree that still
has violations and the baseline is ratcheted down as they are fixed.

Usage::

    python3 scripts/rule_drift.py                    # compare with the baseline
    python3 scripts/rule_drift.py --update-baseline  # rewrite the baseline
    python3 scripts/rule_drift.py --list METRIC      # print every location

Only production code is scanned: ``tests/``, ``benches/``, ``examples/`` and
``build.rs`` are skipped, ``#[cfg(test)]`` items are stripped, and an
out-of-line ``#[cfg(test)] mod x;`` excludes the file it points at. The one
exception is ``#[ignore]``, which only exists in test code, so it is counted
everywhere.

Pure standard-library Python with no build step: it runs in a pre-commit
hook and in CI in a couple of seconds.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

BASELINE_VERSION = 1

# Crates that are binaries first and may use anyhow. Everything else is a
# library crate: its errors cross a crate boundary and must be typed.
ANYHOW_ALLOWED_CRATES = frozenset({"temps-cli"})

FILE_LINE_LIMIT = 3000
FUNCTION_LINE_LIMIT = 300

METRICS: dict[str, str] = {
    "handler_db_access": "sea-orm imports or queries inside handlers/ (use a service)",
    "problem_catch_all": "`_ =>` arms inside `impl From<..> for Problem` (match every variant)",
    "json_value_response": "handlers returning Json<Value> / Json<serde_json::Value> (use a typed struct)",
    "lazy_error_variants": "error variants that are a bare `X(String)` or `#[error(\"{0}\")]` (add context fields)",
    "unguarded_require_auth": "functions taking RequireAuth with no *_guard!/permission_check!/require_*/has_permission check",
    "ignore_without_reason": "bare #[ignore] (runtime-skip Docker/network tests; `#[ignore = \"reason\"]` only for --ignored CI jobs)",
    "ignore_with_reason": "#[ignore = \"reason\"] tests (must be run by a dedicated --ignored CI job)",
    "anyhow_in_library": "anyhow usage in library crates (use a thiserror enum)",
    "files_over_3000_lines": f"production files over {FILE_LINE_LIMIT} lines (split by responsibility)",
    "functions_over_300_lines": f"functions over {FUNCTION_LINE_LIMIT} lines (extract steps)",
}

SKIPPED_DIRS = frozenset({"tests", "benches", "examples", "target", "fuzz"})


@dataclass
class Finding:
    metric: str
    path: str
    line: int
    detail: str = ""


@dataclass
class CrateCounts:
    counts: dict[str, int] = field(default_factory=dict)
    findings: list[Finding] = field(default_factory=list)

    def add(self, finding: Finding) -> None:
        self.counts[finding.metric] = self.counts.get(finding.metric, 0) + 1
        self.findings.append(finding)


# ---------------------------------------------------------------------------
# Source handling
# ---------------------------------------------------------------------------

_CHAR_LITERAL = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[^\\'\n])'")


def mask_source(text: str) -> str:
    """Blank out comments, strings and char literals, keeping offsets.

    Every masked character becomes a space (newlines are kept), so brace
    matching and regexes on the result see only code while line numbers and
    offsets still line up with the original text.
    """
    out = list(text)
    i = 0
    n = len(text)

    def blank(start: int, end: int) -> None:
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if c == "/" and nxt == "/":
            end = text.find("\n", i)
            end = n if end == -1 else end
            blank(i, end)
            i = end
        elif c == "/" and nxt == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and (nxt == '"' or nxt == "#") and not _is_ident_char(text, i - 1):
            m = re.match(r'r(#*)"', text[i:])
            if not m:
                i += 1
                continue
            hashes = m.group(1)
            close = '"' + hashes
            j = text.find(close, i + len(m.group(0)))
            j = n if j == -1 else j + len(close)
            blank(i + 1, j)
            i = j
        elif c == "b" and nxt == '"' and not _is_ident_char(text, i - 1):
            j = _end_of_string(text, i + 1)
            blank(i + 1, j)
            i = j
        elif c == '"':
            j = _end_of_string(text, i)
            blank(i, j)
            i = j
        elif c == "'":
            m = _CHAR_LITERAL.match(text, i)
            if m:
                blank(i, m.end())
                i = m.end()
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


def _is_ident_char(text: str, index: int) -> bool:
    return index >= 0 and (text[index].isalnum() or text[index] == "_")


def _end_of_string(text: str, start: int) -> int:
    """Return the offset just past the string literal opening at `start`."""
    j = start + 1
    n = len(text)
    while j < n:
        if text[j] == "\\":
            j += 2
        elif text[j] == '"':
            return j + 1
        else:
            j += 1
    return n


def matching_brace(masked: str, open_index: int) -> int:
    """Offset of the `}` closing the `{` at `open_index` (or end of text)."""
    depth = 0
    for k in range(open_index, len(masked)):
        ch = masked[k]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return k
    return len(masked) - 1


_CFG_TEST = re.compile(r"^(\s*)#\[cfg\(test\)\]\s*$")
_OUT_OF_LINE_MOD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")


def strip_test_items(text: str) -> tuple[str, list[str]]:
    """Remove `#[cfg(test)]` items, keeping line numbers.

    The item ends at the first line that is exactly the attribute's
    indentation followed by `}` (rustfmt always closes an item at the
    indentation it opened at). Cutting at the first `#[cfg(test)]` instead
    would drop the production code that many files keep after their test
    module. Removed lines are replaced by empty lines.

    Returns the stripped text and the names of out-of-line test modules
    (`#[cfg(test)] mod tests;`), whose files are test code too.
    """
    lines = text.split("\n")
    out: list[str] = []
    test_modules: list[str] = []
    i = 0
    while i < len(lines):
        m = _CFG_TEST.match(lines[i])
        if not m:
            out.append(lines[i])
            i += 1
            continue
        indent = m.group(1)
        j = i + 1
        # Further attributes and doc comments belong to the same item.
        while j < len(lines) and re.match(r"^\s*(#\[|///|//!)", lines[j]):
            j += 1
        if j >= len(lines):
            out.extend([""] * (len(lines) - i))
            break
        item = lines[j]
        if item.rstrip().endswith(";"):
            mod = _OUT_OF_LINE_MOD.match(item)
            if mod:
                test_modules.append(mod.group(1))
            end = j
        elif item.rstrip().endswith("}") and item.count("{") and item.count("{") == item.count("}"):
            end = j  # a one-line item: `fn f() {}`
        else:
            end = j + 1
            closers = {indent + "}", indent + "};", indent + "})"}
            while end < len(lines) and lines[end].rstrip() not in closers:
                end += 1
        out.extend([""] * (end - i + 1))
        i = end + 1
    return "\n".join(out), test_modules


def line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


# ---------------------------------------------------------------------------
# Crate discovery
# ---------------------------------------------------------------------------


@dataclass
class SourceFile:
    crate: str
    path: Path
    rel: str
    production: bool
    is_binary_root: bool


def discover_files(root: Path) -> list[SourceFile]:
    files: list[SourceFile] = []
    crates_dir = root / "crates"
    for manifest in sorted(crates_dir.glob("*/Cargo.toml")):
        crate_dir = manifest.parent
        crate = _crate_name(manifest) or crate_dir.name
        test_files = _out_of_line_test_files(crate_dir)
        for path in sorted(crate_dir.rglob("*.rs")):
            rel_in_crate = path.relative_to(crate_dir)
            parts = rel_in_crate.parts
            if "target" in parts:
                continue
            production = not (
                any(p in SKIPPED_DIRS for p in parts[:-1])
                or path.name == "build.rs"
                or path in test_files
                or any(parent in test_files for parent in _mod_dirs(path, crate_dir))
            )
            is_binary_root = parts[:2] == ("src", "bin") or rel_in_crate == Path("src/main.rs")
            files.append(
                SourceFile(
                    crate=crate,
                    path=path,
                    rel=path.relative_to(root).as_posix(),
                    production=production,
                    is_binary_root=is_binary_root,
                )
            )
    return files


def _crate_name(manifest: Path) -> str | None:
    in_package = False
    for line in manifest.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package:
            m = re.match(r'name\s*=\s*"([^"]+)"', stripped)
            if m:
                return m.group(1)
    return None


def _mod_dirs(path: Path, crate_dir: Path) -> list[Path]:
    """`a/b/c.rs` lives under the module files `a/b.rs` and `a/b/mod.rs`."""
    result = []
    parent = path.parent
    while parent != crate_dir and parent != parent.parent:
        result.append(parent.with_suffix(".rs"))
        result.append(parent / "mod.rs")
        parent = parent.parent
    return result


def _out_of_line_test_files(crate_dir: Path) -> set[Path]:
    """Files declared as `#[cfg(test)] mod x;` anywhere in the crate."""
    found: set[Path] = set()
    for path in crate_dir.rglob("*.rs"):
        if "target" in path.relative_to(crate_dir).parts:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        if "#[cfg(test)]" not in text:
            continue
        _, modules = strip_test_items(text)
        if not modules:
            continue
        base = path.parent if path.name in ("mod.rs", "lib.rs", "main.rs") else path.with_suffix("")
        for name in modules:
            found.add(base / f"{name}.rs")
            found.add(base / name / "mod.rs")
    return found


# ---------------------------------------------------------------------------
# Rules
# ---------------------------------------------------------------------------

_HANDLER_DB = re.compile(
    r"\buse\s+sea_orm\b|\bsea_orm::|\b\w*Entity::find\w*\s*\(|::find_by_id\s*\(|\binto_active_model\s*\("
)
_PROBLEM_IMPL = re.compile(r"\bimpl\b[^{;]*\bFrom\s*<[^{;]*>\s*for\s+(?:\w+::)*Problem\b[^{;]*\{")
_CATCH_ALL = re.compile(r"(?<![\w.])_\s*=>")
_FN_SIGNATURE = re.compile(r"\bfn\s+(\w+)\s*(?:<[^{;]*?>)?\s*\(", re.S)
_JSON_VALUE = re.compile(r"Json\s*<\s*(?:serde_json::)?Value\s*>")
# Any guard macro (`permission_guard!`, `project_scope_guard!`, ...), the
# `require_*` helpers, or an explicit permission/role test counts as an
# authorization decision.
_GUARD = re.compile(
    r"\b\w*_guard!|\bpermission_check!|\brequire_\w+|\.has_permission\s*\(|\.is_admin\s*\(|"
    r"\.is_instance_admin\s*\(|\.has_role\s*\("
)
_ERROR_DERIVE = re.compile(r"#\[derive\([^)]*\bError\b[^)]*\)\]")
_ENUM_HEAD = re.compile(r"\benum\s+\w+[^{;]*\{")
_STRING_VARIANT = re.compile(r"^\s*(\w+)\s*\(\s*(?:std::string::)?String\s*\)\s*,?\s*$")
_ANYHOW = re.compile(r"\banyhow\b")
_IGNORE = re.compile(r"#\[ignore(\s*=\s*\"[^\"]*\"|\s*\])")


def is_handler_file(rel: str) -> bool:
    return "/handlers/" in rel or rel.endswith("/handlers.rs")


def analyze_file(source: SourceFile, text: str) -> list[Finding]:
    findings: list[Finding] = []

    # `#[ignore]` lives in test code, so it is counted before stripping, on
    # the original text (masking would blank the reason string).
    for idx, line in enumerate(text.split("\n"), start=1):
        stripped = line.strip()
        if stripped.startswith("//"):
            continue
        m = _IGNORE.search(line)
        if m:
            metric = "ignore_with_reason" if "=" in m.group(1) else "ignore_without_reason"
            findings.append(Finding(metric, source.rel, idx))

    if not source.production:
        return findings

    stripped_text, _ = strip_test_items(text)
    masked = mask_source(stripped_text)
    lines = masked.split("\n")

    nonblank = sum(1 for line in stripped_text.split("\n") if line.strip())
    if nonblank > FILE_LINE_LIMIT:
        findings.append(Finding("files_over_3000_lines", source.rel, 1, f"{nonblank} lines"))

    if is_handler_file(source.rel):
        for idx, line in enumerate(lines, start=1):
            if _HANDLER_DB.search(line):
                findings.append(Finding("handler_db_access", source.rel, idx, line.strip()[:80]))

    for m in _PROBLEM_IMPL.finditer(masked):
        open_index = m.end() - 1
        close = matching_brace(masked, open_index)
        body = masked[open_index:close]
        for arm in _CATCH_ALL.finditer(body):
            findings.append(Finding("problem_catch_all", source.rel, line_of(masked, open_index + arm.start())))

    for name, sig_start, body_open, body_close in iter_functions(masked):
        signature = masked[sig_start:body_open]
        body = masked[body_open:body_close]
        start_line = line_of(masked, sig_start)
        end_line = line_of(masked, body_close)
        if end_line - start_line + 1 > FUNCTION_LINE_LIMIT:
            findings.append(
                Finding("functions_over_300_lines", source.rel, start_line, f"{name}: {end_line - start_line + 1} lines")
            )
        if "->" in signature and _JSON_VALUE.search(signature.split("->", 1)[1]):
            findings.append(Finding("json_value_response", source.rel, start_line, name))
        if re.search(r"\bRequireAuth\b", signature) and not _GUARD.search(body):
            findings.append(Finding("unguarded_require_auth", source.rel, start_line, name))

    for derive in _ERROR_DERIVE.finditer(masked):
        head = _ENUM_HEAD.search(masked, derive.end())
        if not head:
            continue
        # The derive must apply to this enum: only attributes in between.
        between = masked[derive.end() : head.start()]
        if re.search(r"\b(struct|fn|impl|mod|enum)\b", between):
            continue
        open_index = head.end() - 1
        close = matching_brace(masked, open_index)
        findings.extend(lazy_variants(source.rel, stripped_text, masked, open_index, close))

    if source.crate not in ANYHOW_ALLOWED_CRATES and not source.is_binary_root:
        for idx, line in enumerate(lines, start=1):
            if _ANYHOW.search(line):
                findings.append(Finding("anyhow_in_library", source.rel, idx, line.strip()[:80]))

    return findings


def iter_functions(masked: str):
    """Yield (name, signature_start, body_open, body_close) for each fn body."""
    for m in _FN_SIGNATURE.finditer(masked):
        # Walk the signature to its body `{` or a `;` (trait method/extern).
        depth = 0
        k = m.end() - 1
        body_open = -1
        while k < len(masked):
            ch = masked[k]
            if ch in "(<[":
                depth += 1
            elif ch in ")>]":
                # `->` is not a closing angle bracket.
                if not (ch == ">" and k > 0 and masked[k - 1] == "-"):
                    depth -= 1
            elif ch == ";" and depth <= 0:
                break
            elif ch == "{" and depth <= 0:
                body_open = k
                break
            k += 1
        if body_open == -1:
            continue
        yield m.group(1), m.start(), body_open, matching_brace(masked, body_open)


def lazy_variants(rel: str, original: str, masked: str, open_index: int, close: int) -> list[Finding]:
    findings = []
    body_original = original[open_index + 1 : close]
    base_line = line_of(masked, open_index)
    body_masked = masked[open_index + 1 : close]
    depth = 0
    pending_display_only = False
    for offset, (line, masked_line) in enumerate(zip(body_original.split("\n"), body_masked.split("\n"))):
        if depth == 0:
            stripped = line.strip()
            if re.match(r'#\[error\(\s*"\{0\}"\s*\)\]', stripped):
                pending_display_only = True
            elif stripped.startswith("#[") or stripped.startswith("//") or not stripped:
                pass
            else:
                variant = re.match(r"^\s*(\w+)", masked_line)
                if variant:
                    is_string = bool(_STRING_VARIANT.match(masked_line))
                    if is_string or pending_display_only:
                        findings.append(Finding("lazy_error_variants", rel, base_line + offset, variant.group(1)))
                    pending_display_only = False
        depth += masked_line.count("{") + masked_line.count("(") - masked_line.count("}") - masked_line.count(")")
        depth = max(depth, 0)
    return findings


# ---------------------------------------------------------------------------
# Baseline comparison
# ---------------------------------------------------------------------------


def collect(root: Path) -> dict[str, CrateCounts]:
    results: dict[str, CrateCounts] = {}
    for source in discover_files(root):
        try:
            text = source.path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            print(f"warning: cannot read {source.rel}: {error}", file=sys.stderr)
            continue
        crate = results.setdefault(source.crate, CrateCounts())
        for finding in analyze_file(source, text):
            crate.add(finding)
    return results


def to_baseline(results: dict[str, CrateCounts]) -> dict:
    crates = {}
    for crate in sorted(results):
        counts = {metric: results[crate].counts[metric] for metric in METRICS if results[crate].counts.get(metric)}
        if counts:
            crates[crate] = counts
    return {
        "version": BASELINE_VERSION,
        "description": "Per-crate CLAUDE.md rule-violation counts. CI fails when a count rises; "
        "lower it with `python3 scripts/rule_drift.py --update-baseline` when you fix violations.",
        "metrics": METRICS,
        "crates": crates,
    }


def compare(results: dict[str, CrateCounts], baseline: dict) -> tuple[list[str], list[str]]:
    """Return (regressions, improvements) as printable lines."""
    regressions: list[str] = []
    improvements: list[str] = []
    base_crates: dict[str, dict[str, int]] = baseline.get("crates", {})
    for crate in sorted(set(results) | set(base_crates)):
        current = results.get(crate, CrateCounts()).counts
        allowed = base_crates.get(crate, {})
        for metric in METRICS:
            now = current.get(metric, 0)
            before = allowed.get(metric, 0)
            if now > before:
                regressions.append(f"{crate}: {metric} rose from {before} to {now} -- {METRICS[metric]}")
                for finding in results[crate].findings:
                    if finding.metric == metric:
                        detail = f" ({finding.detail})" if finding.detail else ""
                        regressions.append(f"    {finding.path}:{finding.line}{detail}")
            elif now < before:
                improvements.append(f"{crate}: {metric} fell from {before} to {now}")
    return regressions, improvements


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--baseline", type=Path, default=None)
    parser.add_argument("--update-baseline", action="store_true", help="rewrite the baseline with the current counts")
    parser.add_argument("--list", metavar="METRIC", choices=sorted(METRICS), help="print every location for one metric")
    args = parser.parse_args(argv)

    root: Path = args.root
    baseline_path: Path = args.baseline or root / "scripts" / "rule-baseline.json"
    results = collect(root)

    if args.list:
        for crate in sorted(results):
            for finding in results[crate].findings:
                if finding.metric == args.list:
                    detail = f"  {finding.detail}" if finding.detail else ""
                    print(f"{finding.path}:{finding.line}{detail}")
        return 0

    if args.update_baseline:
        baseline_path.write_text(json.dumps(to_baseline(results), indent=2, sort_keys=False) + "\n", encoding="utf-8")
        print(f"Wrote {baseline_path.relative_to(root) if baseline_path.is_relative_to(root) else baseline_path}")
        return 0

    try:
        baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        print(f"error: baseline {baseline_path} does not exist; run with --update-baseline", file=sys.stderr)
        return 2
    except json.JSONDecodeError as error:
        print(f"error: baseline {baseline_path} is not valid JSON: {error}", file=sys.stderr)
        return 2
    if baseline.get("version") != BASELINE_VERSION:
        print(f"error: baseline {baseline_path} has version {baseline.get('version')}, expected {BASELINE_VERSION}", file=sys.stderr)
        return 2

    regressions, improvements = compare(results, baseline)
    if improvements:
        print("Counts fell below the baseline -- lock the gain in with --update-baseline:")
        for line in improvements:
            print(f"  {line}")
    if regressions:
        print("Rule drift: these counts rose above scripts/rule-baseline.json:", file=sys.stderr)
        for line in regressions:
            print(f"  {line}", file=sys.stderr)
        print(
            "\nFix the new violations (see CLAUDE.md). If one is a deliberate, reviewed exception, "
            "run `python3 scripts/rule_drift.py --update-baseline` and explain it in the PR.",
            file=sys.stderr,
        )
        return 1
    total = sum(sum(c.counts.values()) for c in results.values())
    print(f"Rule drift: OK ({len(results)} crates, {total} known violations, none new)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
