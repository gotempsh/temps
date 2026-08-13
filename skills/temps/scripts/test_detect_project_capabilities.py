#!/usr/bin/env python3
"""Regression tests for the read-only capability detector."""

from __future__ import annotations

import os
import tempfile
import threading
import unittest
from pathlib import Path
from typing import Any, cast

from detect_project_capabilities import detect


class CapabilityDetectorTests(unittest.TestCase):
    def test_dependency_without_initialization_is_partial(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "package.json").write_text(
                '{"dependencies":{"@sentry/nextjs":"1.0.0"}}', encoding="utf-8"
            )

            result = detect(root)

            self.assertEqual(result["capabilities"]["error_tracking"]["status"], "partial")

    def test_source_initialization_is_configured(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "package.json").write_text(
                '{"dependencies":{"@opentelemetry/sdk-node":"1.0.0"}}', encoding="utf-8"
            )
            (root / "instrumentation.ts").write_text(
                "const sdk = new NodeSDK({}); sdk.start();", encoding="utf-8"
            )

            result = detect(root)

            self.assertEqual(result["capabilities"]["tracing"]["status"], "configured")

    def test_environment_placeholder_is_not_configuration(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "compose.yaml").write_text(
                "environment:\n  SENTRY_DSN: ${SENTRY_DSN}\n  OTEL_EXPORTER_OTLP_ENDPOINT: ${OTEL_ENDPOINT}\n",
                encoding="utf-8",
            )

            result = detect(root)

            self.assertEqual(result["capabilities"]["error_tracking"]["status"], "missing")
            self.assertEqual(result["capabilities"]["tracing"]["status"], "missing")

    def test_dotnet_project_is_detected_by_suffix(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Example.csproj").write_text("<Project />", encoding="utf-8")

            result = detect(root)

            self.assertIn("dotnet", [item["name"] for item in result["frameworks"]])

    def test_symlinked_file_outside_root_is_not_read(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            root = workspace / "project"
            root.mkdir()
            outside = workspace / "outside.ts"
            outside.write_text("Sentry.init({});", encoding="utf-8")
            (root / "instrumentation.ts").symlink_to(outside)

            result = detect(root)

            self.assertEqual(result["capabilities"]["error_tracking"]["status"], "missing")
            self.assertEqual(
                result["capabilities"]["error_tracking"]["initialization_evidence"],
                [],
            )

    def test_symlinked_directory_outside_root_is_not_read(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            root = workspace / "project"
            root.mkdir()
            outside = workspace / "outside"
            outside.mkdir()
            (outside / "package.json").write_text(
                '{"dependencies":{"@sentry/nextjs":"1.0.0"}}', encoding="utf-8"
            )
            (root / "linked").symlink_to(outside, target_is_directory=True)

            result = detect(root)

            self.assertEqual(result["capabilities"]["error_tracking"]["status"], "missing")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO files are not supported")
    def test_named_pipe_is_ignored_without_blocking(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            os.mkfifo(root / "package.json")
            outcome: dict[str, object] = {}

            def run_detection() -> None:
                outcome["result"] = detect(root)

            detector = threading.Thread(target=run_detection, daemon=True)
            detector.start()
            detector.join(timeout=1)

            self.assertFalse(detector.is_alive(), "capability detection blocked on a FIFO")
            result = cast(dict[str, Any], outcome["result"])
            self.assertIsInstance(result, dict)
            self.assertEqual(
                result["capabilities"]["error_tracking"]["status"],
                "missing",
            )


if __name__ == "__main__":
    unittest.main()
