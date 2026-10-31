#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for crate_map.py. They feed synthetic `cargo metadata` and never run cargo."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import crate_map  # noqa: E402

ROOT = "/workspace"


def package(name: str, deps: list[tuple[str, str | None]] = (), *, description: str | None = "does things",
            bins: list[str] = (), directory: str = "crates") -> dict:
    return {
        "id": f"path+file://{ROOT}/{directory}/{name}#0.1.0",
        "name": name,
        "description": description,
        "manifest_path": f"{ROOT}/{directory}/{name}/Cargo.toml",
        "targets": [{"name": name.replace("-", "_"), "kind": ["lib"]}]
        + [{"name": b, "kind": ["bin"]} for b in bins],
        "dependencies": [
            {"name": dep, "kind": kind, "path": f"{ROOT}/crates/{dep}" if dep.startswith("temps-") else None}
            for dep, kind in deps
        ],
    }


def metadata(*packages: dict, outside: tuple[dict, ...] = ()) -> dict:
    return {
        "workspace_root": ROOT,
        "workspace_members": [p["id"] for p in packages],
        "packages": [*packages, *outside],
    }


SAMPLE = metadata(
    package("temps-core", [("serde", None)]),
    package("temps-auth", [("temps-core", None)]),
    package("temps-proxy", [("temps-core", None), ("temps-auth", None), ("temps-flags", "dev")]),
    package("temps-flags", [("temps-core", None), ("temps-auth", "build")]),
    package("temps-cli", [("temps-proxy", None), ("temps-flags", None)], bins=["temps"]),
)

LAYERS = {
    "temps-core": "foundation",
    "temps-auth": "foundation",
    "temps-proxy": "traffic",
    "temps-flags": "domain",
    "temps-cli": "application",
}


class BuildTests(unittest.TestCase):
    def test_consumers_are_direct_non_dev_internal_dependents(self) -> None:
        crates = crate_map.build_crates(SAMPLE)
        self.assertEqual(crates["temps-core"].consumers, {"temps-auth", "temps-proxy", "temps-flags"})
        # A dev-dependency is test wiring, not a consumer; a build-dependency is one.
        self.assertEqual(crates["temps-flags"].consumers, {"temps-cli"})
        self.assertEqual(crates["temps-auth"].consumers, {"temps-proxy", "temps-flags"})
        # External crates never appear.
        self.assertEqual(crates["temps-core"].dependencies, set())

    def test_binaries_and_paths(self) -> None:
        crates = crate_map.build_crates(SAMPLE)
        self.assertEqual(crates["temps-cli"].binaries, ["temps"])
        self.assertEqual(crates["temps-cli"].path, "crates/temps-cli")

    def test_non_members_are_ignored(self) -> None:
        data = metadata(package("temps-core"), outside=(package("temps-vendored", [("temps-core", None)]),))
        crates = crate_map.build_crates(data)
        self.assertEqual(set(crates), {"temps-core"})
        self.assertEqual(crates["temps-core"].consumers, set())


class ValidateTests(unittest.TestCase):
    def test_missing_description_fails_with_the_manifest_path(self) -> None:
        data = metadata(package("temps-core", description=None))
        with self.assertRaises(crate_map.CrateMapError) as raised:
            crate_map.validate(crate_map.build_crates(data), {"temps-core": "foundation"})
        self.assertIn("crates/temps-core/Cargo.toml", str(raised.exception))

    def test_blank_description_counts_as_missing(self) -> None:
        data = metadata(package("temps-core", description="  "))
        with self.assertRaises(crate_map.CrateMapError):
            crate_map.validate(crate_map.build_crates(data), {"temps-core": "foundation"})

    def test_unmapped_crate_fails(self) -> None:
        with self.assertRaises(crate_map.CrateMapError) as raised:
            crate_map.validate(crate_map.build_crates(SAMPLE), {**LAYERS, "temps-cli": "nowhere"})
        self.assertIn("unknown layer", str(raised.exception))
        layers = dict(LAYERS)
        del layers["temps-flags"]
        with self.assertRaises(crate_map.CrateMapError) as raised:
            crate_map.validate(crate_map.build_crates(SAMPLE), layers)
        self.assertIn("temps-flags has no layer", str(raised.exception))

    def test_stale_mapping_entry_fails(self) -> None:
        with self.assertRaises(crate_map.CrateMapError) as raised:
            crate_map.validate(crate_map.build_crates(SAMPLE), {**LAYERS, "temps-gone": "domain"})
        self.assertIn("temps-gone, which is not a workspace member", str(raised.exception))

    def test_every_layer_has_a_tier_and_a_description(self) -> None:
        self.assertEqual(set(crate_map.LAYER_TIERS), set(crate_map.LAYER_DESCRIPTIONS))


class RenderTests(unittest.TestCase):
    def render(self, layers: dict[str, str] = LAYERS) -> str:
        crates = crate_map.build_crates(SAMPLE)
        crate_map.validate(crates, layers)
        return crate_map.render(crates, layers)

    def test_rows_show_description_consumers_and_binaries(self) -> None:
        text = self.render()
        self.assertIn(
            "| [`temps-core`](../../crates/temps-core) | does things | **3**: auth, flags, proxy |  |", text
        )
        self.assertIn("| [`temps-cli`](../../crates/temps-cli) | does things | none | `temps` |", text)

    def test_upward_edges_are_listed(self) -> None:
        text = self.render({**LAYERS, "temps-auth": "domain"})
        self.assertIn("| `temps-proxy` (traffic) | `temps-auth` (domain) |", text)

    def test_no_upward_edges_says_none(self) -> None:
        self.assertIn("## Edges against the layer order", self.render())
        self.assertTrue(self.render().rstrip().endswith("None."))

    def test_render_is_deterministic(self) -> None:
        self.assertEqual(self.render(), self.render())


class MainTests(unittest.TestCase):
    def run_main(self, root: Path, *args: str) -> tuple[int, str]:
        meta = root / "metadata.json"
        meta.write_text(json.dumps(SAMPLE), encoding="utf-8")
        out = io.StringIO()
        with mock.patch.object(crate_map, "LAYERS", LAYERS), contextlib.redirect_stdout(out), \
                contextlib.redirect_stderr(out):
            code = crate_map.main(["--root", str(root), "--metadata", str(meta), *args])
        return code, out.getvalue()

    def test_write_then_check_passes_and_edit_makes_it_stale(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertEqual(self.run_main(root, "--check")[0], 1)
            self.assertEqual(self.run_main(root)[0], 0)
            self.assertEqual(self.run_main(root, "--check")[0], 0)
            output = root / crate_map.OUTPUT
            output.write_text(output.read_text(encoding="utf-8") + "hand edit\n", encoding="utf-8")
            code, message = self.run_main(root, "--check")
            self.assertEqual(code, 1)
            self.assertIn("out of date", message)


if __name__ == "__main__":
    unittest.main()
