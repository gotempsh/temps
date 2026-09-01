#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for source_attribution.py."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import source_attribution


class SourceAttributionTests(unittest.TestCase):
    def test_rust_header_is_added_before_inner_attributes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lib.rs"
            path.write_text("#![forbid(unsafe_code)]\npub fn example() {}\n", encoding="utf-8")

            self.assertTrue(source_attribution.annotate_file(path))

            contents = path.read_text(encoding="utf-8")
            self.assertTrue(source_attribution.has_attribution(contents))
            self.assertLess(contents.index("SPDX-FileCopyrightText"), contents.index("#![forbid"))

    def test_shell_header_preserves_shebang_as_first_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "example.sh"
            path.write_text("#!/bin/sh\necho hello\n", encoding="utf-8")

            source_attribution.annotate_file(path)

            contents = path.read_text(encoding="utf-8")
            self.assertEqual(contents.splitlines()[0], "#!/bin/sh")
            self.assertTrue(source_attribution.has_attribution(contents))

    def test_existing_header_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "example.ts"
            path.write_text(
                "// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n"
                "// SPDX-License-Identifier: MIT OR Apache-2.0\n\n"
                "export const answer = 42\n",
                encoding="utf-8",
            )

            self.assertFalse(source_attribution.annotate_file(path))

    def test_whitespace_only_file_does_not_keep_trailing_blank_lines(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "empty.rs"
            path.write_text("\n", encoding="utf-8")

            source_attribution.annotate_file(path)

            self.assertEqual(
                path.read_text(encoding="utf-8"),
                "// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n"
                "// SPDX-License-Identifier: MIT OR Apache-2.0\n",
            )

    def test_different_spdx_header_requires_manual_review(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "third_party.js"
            path.write_text(
                "// SPDX-FileCopyrightText: 2020 Third Party\n"
                "// SPDX-License-Identifier: MIT\n",
                encoding="utf-8",
            )

            with self.assertRaises(source_attribution.AttributionError):
                source_attribution.annotate_file(path)

    def test_html_header_preserves_doctype_as_first_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "index.html"
            path.write_text("<!doctype html>\n<title>Example</title>\n", encoding="utf-8")

            source_attribution.annotate_file(path)

            contents = path.read_text(encoding="utf-8")
            self.assertEqual(contents.splitlines()[0], "<!doctype html>")
            self.assertTrue(source_attribution.has_attribution(contents))


if __name__ == "__main__":
    unittest.main()
