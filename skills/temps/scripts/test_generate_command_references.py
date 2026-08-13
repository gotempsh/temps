#!/usr/bin/env python3
"""Regression tests for the command-reference splitter."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from generate_command_references import split_catalog


class CommandReferenceGeneratorTests(unittest.TestCase):
    def test_splits_groups_and_excludes_monolithic_footer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "COMMANDS.md"
            destination = root / "commands"
            source.write_text(
                "# CLI\n\n## `alpha`\n\nAlpha.\n\n## `beta`\n\nBeta.\n\n"
                "---\n\n## Examples\n\nNot a command group.\n",
                encoding="utf-8",
            )

            generated = split_catalog(source, destination)

            self.assertEqual([command for command, _ in generated], ["alpha", "beta"])
            self.assertNotIn("## Examples", (destination / "beta.md").read_text(encoding="utf-8"))
            self.assertIn("[`alpha`](alpha.md)", (destination / "INDEX.md").read_text(encoding="utf-8"))

    def test_adds_contents_to_a_long_group(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "COMMANDS.md"
            destination = root / "commands"
            body = "\n".join(f"line {number}" for number in range(301))
            source.write_text(
                f"# CLI\n\n## `services`\n\n### `services list`\n\n{body}\n",
                encoding="utf-8",
            )

            split_catalog(source, destination)

            output = (destination / "services.md").read_text(encoding="utf-8")
            self.assertIn("## Contents", output)
            self.assertIn("[`services list`](#services-list)", output)

    def test_rejects_nonempty_noncanonical_destination(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "COMMANDS.md"
            destination = root / "docs"
            destination.mkdir()
            (destination / "handwritten.md").write_text("keep me", encoding="utf-8")
            source.write_text("# CLI\n\n## `alpha`\n\nAlpha.\n", encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
                split_catalog(source, destination)

            self.assertEqual(
                (destination / "handwritten.md").read_text(encoding="utf-8"),
                "keep me",
            )

    def test_rejects_symlink_destination(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "COMMANDS.md"
            target = root / "target"
            target.mkdir()
            destination = root / "commands"
            destination.symlink_to(target, target_is_directory=True)
            source.write_text("# CLI\n\n## `alpha`\n\nAlpha.\n", encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "must not be a symlink"):
                split_catalog(source, destination)

    def test_canonical_cleanup_only_removes_generated_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "COMMANDS.md"
            destination = root / "commands"
            destination.mkdir()
            stale = destination / "stale.md"
            stale.write_text(
                "<!-- Generated from skills/temps-cli/references/COMMANDS.md. "
                "Do not edit manually. -->\n\nstale\n",
                encoding="utf-8",
            )
            handwritten = destination / "handwritten.md"
            handwritten.write_text("keep me", encoding="utf-8")
            source.write_text("# CLI\n\n## `alpha`\n\nAlpha.\n", encoding="utf-8")

            split_catalog(
                source,
                destination,
                canonical_destination=destination,
            )

            self.assertFalse(stale.exists())
            self.assertEqual(handwritten.read_text(encoding="utf-8"), "keep me")

    def test_rejects_symlinked_generated_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "COMMANDS.md"
            destination = root / "commands"
            destination.mkdir()
            outside = root / "outside.md"
            outside.write_text("keep me", encoding="utf-8")
            (destination / "alpha.md").symlink_to(outside)
            source.write_text("# CLI\n\n## `alpha`\n\nAlpha.\n", encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "must not be a symlink"):
                split_catalog(
                    source,
                    destination,
                    canonical_destination=destination,
                )

            self.assertEqual(outside.read_text(encoding="utf-8"), "keep me")


if __name__ == "__main__":
    unittest.main()
