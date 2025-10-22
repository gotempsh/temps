#!/usr/bin/env python3
"""
Validate CHANGELOG.md follows Keep a Changelog format.
https://keepachangelog.com/en/1.0.0/
"""

import re
import sys
from pathlib import Path

# ANSI color codes
RED = '\033[0;31m'
GREEN = '\033[0;32m'
YELLOW = '\033[1;33m'
NC = '\033[0m'  # No Color

VALID_CATEGORIES = ['Added', 'Changed', 'Deprecated', 'Removed', 'Fixed', 'Security']

def error(msg):
    print(f"{RED}❌ {msg}{NC}")

def warning(msg):
    print(f"{YELLOW}⚠️  {msg}{NC}")

def success(msg):
    print(f"{GREEN}✅ {msg}{NC}")

def validate_changelog(changelog_path):
    """Validate CHANGELOG.md format."""

    errors = []
    warnings_list = []

    if not changelog_path.exists():
        error("CHANGELOG.md not found")
        return False

    content = changelog_path.read_text()
    lines = content.split('\n')

    print("🔍 Validating CHANGELOG.md format...")

    # 1. Check first line is "# Changelog"
    if not lines[0].startswith('# Changelog'):
        errors.append("First line must be '# Changelog'")

    # 2. Check for Keep a Changelog reference
    if 'keepachangelog.com' not in content:
        warnings_list.append("Missing Keep a Changelog reference")

    # 3. Check for Semantic Versioning reference
    if 'semver.org' not in content:
        warnings_list.append("Missing Semantic Versioning reference")

    # 4. Find all version sections
    version_pattern = re.compile(r'^## \[([^\]]+)\](?:\s+-\s+(\d{4}-\d{2}-\d{2}))?', re.MULTILINE)
    versions = version_pattern.findall(content)

    if not versions:
        errors.append("No version sections found (expected at least ## [Unreleased])")
    else:
        # Check for [Unreleased] section
        if not any(v[0] == 'Unreleased' for v in versions):
            errors.append("Missing ## [Unreleased] section")

        # Validate version sections have dates (except Unreleased)
        for version, date in versions:
            if version != 'Unreleased' and not date:
                warnings_list.append(f"Version [{version}] is missing a date")

            # Validate date format if present
            if date and not re.match(r'\d{4}-\d{2}-\d{2}', date):
                errors.append(f"Version [{version}] has invalid date format: {date} (expected YYYY-MM-DD)")

    # 5. Check for valid categories under [Unreleased]
    unreleased_match = re.search(r'## \[Unreleased\](.*?)(?=## \[|$)', content, re.DOTALL)
    if unreleased_match:
        unreleased_content = unreleased_match.group(1)

        # Check for category headers
        category_pattern = re.compile(r'^### (\w+)', re.MULTILINE)
        categories = category_pattern.findall(unreleased_content)

        if not categories:
            warnings_list.append("[Unreleased] section has no categories (Added/Changed/Fixed/etc.)")
        else:
            # Check for invalid categories
            invalid_categories = [c for c in categories if c not in VALID_CATEGORIES]
            if invalid_categories:
                warnings_list.append(f"Invalid categories in [Unreleased]: {', '.join(invalid_categories)}")
                warnings_list.append(f"Valid categories: {', '.join(VALID_CATEGORIES)}")

            # Check if categories have entries
            has_entries = bool(re.search(r'^- \S', unreleased_content, re.MULTILINE))
            if not has_entries:
                warnings_list.append("[Unreleased] section appears to be empty (no bullet points)")

    # 6. Check for proper list formatting
    # Lists should use "- " not "* " or "+ "
    if re.search(r'^[\*\+] ', content, re.MULTILINE):
        warnings_list.append("Use '- ' for lists, not '* ' or '+ '")

    # 7. Check for comparison links at bottom
    if versions and len(versions) > 1:
        # Should have links like: [Unreleased]: https://github.com/...
        if not re.search(r'^\[Unreleased\]: https?://', content, re.MULTILINE):
            warnings_list.append("Missing comparison link for [Unreleased]")

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
