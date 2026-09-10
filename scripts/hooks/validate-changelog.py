#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""
Sanity-check the generated CHANGELOG.md.

CHANGELOG.md is generated from Conventional Commits by git-cliff (see cliff.toml)
— it is NOT hand-edited. This hook is a lightweight guard that the generated file
isn't corrupted or emptied; it does NOT enforce a hand-authored structure.

Notably, `## [Unreleased]` is OPTIONAL: git-cliff only emits it when there are
commits since the last tag, so a freshly-released changelog legitimately has none.
"""

import re
import sys
from pathlib import Path

# ANSI color codes
RED = '\033[0;31m'
GREEN = '\033[0;32m'
YELLOW = '\033[1;33m'
NC = '\033[0m'  # No Color


def error(msg):
    print(f"{RED}❌ {msg}{NC}")


def warning(msg):
    print(f"{YELLOW}⚠️  {msg}{NC}")


def success(msg):
    print(f"{GREEN}✅ {msg}{NC}")


def validate_changelog(changelog_path):
    """Validate that the generated CHANGELOG.md is well-formed."""

    errors = []
    warnings_list = []

    if not changelog_path.exists():
        error("CHANGELOG.md not found")
        return False

    content = changelog_path.read_text()
    lines = content.split('\n')

    print("🔍 Validating generated CHANGELOG.md...")

    # 1. Header from cliff.toml
    if not lines[0].startswith('# Changelog'):
        errors.append("First line must be '# Changelog'")
    if 'keepachangelog.com' not in content:
        warnings_list.append("Missing Keep a Changelog reference")
    if 'semver.org' not in content:
        warnings_list.append("Missing Semantic Versioning reference")

    # 2. Version sections. [Unreleased] is optional (only present when there are
    #    unreleased commits), so we only require that *some* section exists.
    version_pattern = re.compile(r'^## \[([^\]]+)\](?:\s+-\s+(\d{4}-\d{2}-\d{2}))?', re.MULTILINE)
    versions = version_pattern.findall(content)

    if not versions:
        errors.append("No version sections found — did git-cliff produce an empty changelog?")
    else:
        for version, date in versions:
            # Every real release must carry a date; only [Unreleased] may omit it.
            if version != 'Unreleased' and not date:
                errors.append(f"Version [{version}] is missing a date (expected YYYY-MM-DD)")
            if date and not re.match(r'\d{4}-\d{2}-\d{2}', date):
                errors.append(f"Version [{version}] has invalid date format: {date}")

    # 3. List formatting: git-cliff emits "- "; flag stray "* "/"+ ".
    if re.search(r'^[\*\+] ', content, re.MULTILINE):
        warnings_list.append("Use '- ' for lists, not '* ' or '+ '")

    # Print results
    print()
    for warning_msg in warnings_list:
        warning(warning_msg)

    if errors:
        print()
        for error_msg in errors:
            error(error_msg)
        print()
        error("CHANGELOG.md validation failed")
        print("Note: CHANGELOG.md is generated — regenerate it with "
              "`scripts/changelog.sh` rather than editing by hand.")
        return False

    if warnings_list:
        print()
        warning("CHANGELOG.md has warnings but is valid")
    else:
        print()
        success("CHANGELOG.md format is valid")

    return True


if __name__ == '__main__':
    changelog_path = Path('CHANGELOG.md')
    sys.exit(0 if validate_changelog(changelog_path) else 1)
