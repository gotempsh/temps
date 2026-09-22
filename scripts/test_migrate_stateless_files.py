# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("migration", Path(__file__).with_name("migrate-stateless-files.py"))
migration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(migration)


class MigrationMappingTests(unittest.TestCase):
    def test_uses_distinct_raw_path_and_content_addressed_namespaces(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = {
                "cas/blobs/aa/bb/hash": "static-assets/blobs/aa/bb/hash",
                "cas/cache/url": "static-assets/paths/url",
                "static/project/index.html": "static-assets/paths/project/index.html",
                "static/screenshots/project/image.png": "static-assets/paths/screenshots/project/image.png",
                "logs/job.jsonl": "logs/build-logs/job.jsonl",
            }
            for name in paths:
                source = root / name
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text("fixture")
            self.assertEqual({p.relative_to(root).as_posix(): key for p, key in migration.mappings(root, root / "logs")}, paths)

    def test_never_follows_symlinks_outside_the_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "static").mkdir()
            (root / "private").write_text("must not migrate")
            (root / "static" / "link").symlink_to(root / "private")
            with self.assertRaisesRegex(ValueError, "Refusing symlink"):
                list(migration.mappings(root, root / "logs"))


if __name__ == "__main__":
    unittest.main()
