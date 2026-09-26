# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("migration", Path(__file__).with_name("migrate-stateless-files.py"))
migration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(migration)


class MigrationMappingTests(unittest.TestCase):
    def test_rejects_collisions_before_any_upload_in_apply_or_dry_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, content in {
                "cas/blobs/first": "would otherwise upload first",
                "cas/cache/project/index.html": "cached asset",
                "static/project/index.html": "different static asset",
            }.items():
                source = root / name
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text(content)
            environment = {
                "TEMPS_LOG_S3_BUCKET": "migration-test",
                "TEMPS_LOG_S3_ACCESS_KEY_ID": "test-key",
                "TEMPS_LOG_S3_SECRET_ACCESS_KEY": "test-secret",
            }
            for apply in (False, True):
                args = ["migrate-stateless-files.py", "--data-dir", str(root),
                        "--log-dir", str(root / "logs"), "--instance-id", "test"]
                if apply:
                    args += ["--apply", "--source-stopped"]
                with self.subTest(apply=apply), \
                     patch("sys.argv", args), \
                     patch.dict(migration.os.environ, environment, clear=True), \
                     patch.object(migration.subprocess, "run") as upload, \
                     patch.object(migration.subprocess, "Popen") as download:
                    with self.assertRaisesRegex(ValueError, "Destination collision") as error:
                        migration.main()
                    self.assertIn("static-assets/paths/project/index.html", str(error.exception))
                    self.assertIn(str(root / "cas/cache/project/index.html"), str(error.exception))
                    self.assertIn(str(root / "static/project/index.html"), str(error.exception))
                    upload.assert_not_called()
                    download.assert_not_called()

    def test_rejects_duplicate_destinations_even_when_bytes_match(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("cas/cache/same", "static/same"):
                source = root / name
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text("identical")
            with self.assertRaisesRegex(ValueError, "Destination collision"):
                migration.mappings(root, root / "logs")

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

    def test_rejects_a_symlink_at_a_source_namespace_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "private").mkdir()
            (root / "private" / "secret").write_text("must not migrate")
            (root / "static").symlink_to(root / "private", target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "Refusing symlink"):
                migration.mappings(root, root / "logs")


if __name__ == "__main__":
    unittest.main()
