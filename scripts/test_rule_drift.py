#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Regression tests for rule_drift.py."""

from __future__ import annotations

import json
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import rule_drift  # noqa: E402


def write_crate(root: Path, name: str, files: dict[str, str]) -> None:
    crate = root / "crates" / name
    crate.mkdir(parents=True)
    (crate / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n', encoding="utf-8")
    for rel, body in files.items():
        path = crate / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(body), encoding="utf-8")


def counts(root: Path) -> dict[str, dict[str, int]]:
    return {crate: dict(c.counts) for crate, c in rule_drift.collect(root).items()}


class StripTestItemsTests(unittest.TestCase):
    def test_production_code_after_test_module_is_kept(self) -> None:
        text = textwrap.dedent(
            """\
            fn before() {}

            #[cfg(test)]
            mod tests {
                fn helper() {
                    let s = "}";
                }
            }

            fn after() { let x = 1; }
            """
        )
        stripped, modules = rule_drift.strip_test_items(text)
        self.assertIn("fn before", stripped)
        self.assertIn("fn after", stripped)
        self.assertNotIn("helper", stripped)
        self.assertEqual(modules, [])
        # Line numbers survive stripping.
        self.assertEqual(len(stripped.split("\n")), len(text.split("\n")))

    def test_nested_test_module_uses_its_own_indentation(self) -> None:
        text = textwrap.dedent(
            """\
            mod outer {
                #[cfg(test)]
                mod tests {
                    fn t() {}
                }
                pub fn kept() {}
            }
            """
        )
        stripped, _ = rule_drift.strip_test_items(text)
        self.assertIn("pub fn kept", stripped)
        self.assertNotIn("fn t()", stripped)

    def test_out_of_line_test_module_is_reported(self) -> None:
        stripped, modules = rule_drift.strip_test_items("#[cfg(test)]\nmod tests;\nfn f() {}\n")
        self.assertEqual(modules, ["tests"])
        self.assertIn("fn f", stripped)

    def test_attributes_between_cfg_and_item_are_stripped(self) -> None:
        text = "#[cfg(test)]\n#[allow(dead_code)]\nfn only_in_tests() {\n    1;\n}\nfn prod() {}\n"
        stripped, _ = rule_drift.strip_test_items(text)
        self.assertNotIn("only_in_tests", stripped)
        self.assertIn("fn prod", stripped)


class MaskSourceTests(unittest.TestCase):
    def test_braces_in_strings_comments_and_chars_are_masked(self) -> None:
        text = 'fn f() { let a = "{"; let b = \'}\'; // {\n let c = r#"}"#; /* { */ }'
        masked = rule_drift.mask_source(text)
        self.assertEqual(len(masked), len(text))
        self.assertEqual(masked.count("{"), 1)
        self.assertEqual(masked.count("}"), 1)

    def test_lifetimes_are_not_char_literals(self) -> None:
        text = "fn f<'a>(x: &'a str) -> &'a str { x }"
        masked = rule_drift.mask_source(text)
        self.assertIn("{ x }", masked)


class RuleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_each_rule_is_counted_in_production_code(self) -> None:
        write_crate(
            self.root,
            "temps-demo",
            {
                "src/lib.rs": """\
                    use anyhow::Result;

                    #[derive(Debug, thiserror::Error)]
                    pub enum DemoError {
                        #[error("Demo {id} not found")]
                        NotFound { id: i32 },
                        #[error("Internal: {0}")]
                        Internal(String),
                        #[error("{0}")]
                        Passthrough(std::io::Error),
                    }

                    impl From<DemoError> for Problem {
                        fn from(e: DemoError) -> Self {
                            match e {
                                DemoError::NotFound { .. } => not_found(),
                                _ => internal(),
                            }
                        }
                    }
                    """,
                "src/handlers/demo.rs": """\
                    use sea_orm::EntityTrait;

                    async fn guarded(RequireAuth(auth): RequireAuth) -> Result<Json<Typed>, Problem> {
                        permission_guard!(auth, DemoRead);
                        todo_ok()
                    }

                    async fn unguarded(RequireAuth(auth): RequireAuth) -> Result<Json<serde_json::Value>, Problem> {
                        let rows = demo::Entity::find().all(db).await?;
                        Ok(Json(json!({})))
                    }
                    """,
                "src/main.rs": "fn main() -> anyhow::Result<()> { Ok(()) }\n",
                "tests/it.rs": "#[test]\n#[ignore]\nfn needs_docker() { let _ = anyhow::anyhow!(\"x\"); }\n",
            },
        )
        got = counts(self.root)["temps-demo"]
        self.assertEqual(got.get("handler_db_access"), 2)
        self.assertEqual(got.get("problem_catch_all"), 1)
        self.assertEqual(got.get("json_value_response"), 1)
        self.assertEqual(got.get("lazy_error_variants"), 2)
        self.assertEqual(got.get("unguarded_require_auth"), 1)
        # tests/ and src/main.rs do not count for anyhow; `#[ignore]` does.
        self.assertEqual(got.get("anyhow_in_library"), 1)
        self.assertEqual(got.get("ignore_without_reason"), 1)

    def test_test_code_is_not_counted(self) -> None:
        write_crate(
            self.root,
            "temps-demo",
            {
                "src/lib.rs": """\
                    pub fn prod() {}

                    #[cfg(test)]
                    mod tests;

                    #[cfg(test)]
                    mod inline {
                        use anyhow::Result;
                        #[test]
                        #[ignore = "run by the docker CI job with --ignored"]
                        fn t() {}
                    }
                    """,
                "src/tests.rs": "use anyhow::Context;\n",
            },
        )
        got = counts(self.root)["temps-demo"]
        self.assertNotIn("anyhow_in_library", got)
        self.assertEqual(got.get("ignore_with_reason"), 1)

    def test_anyhow_allowed_in_binary_crate(self) -> None:
        write_crate(self.root, "temps-cli", {"src/lib.rs": "use anyhow::Result;\n"})
        self.assertNotIn("anyhow_in_library", counts(self.root).get("temps-cli", {}))

    def test_long_function_and_file_are_reported(self) -> None:
        body = "\n".join("    let _ = 1;" for _ in range(rule_drift.FUNCTION_LINE_LIMIT))
        filler = "\n".join(f"const C{i}: u8 = 0;" for i in range(rule_drift.FILE_LINE_LIMIT))
        write_crate(self.root, "temps-demo", {"src/lib.rs": f"fn long() {{\n{body}\n}}\n{filler}\n"})
        got = counts(self.root)["temps-demo"]
        self.assertEqual(got.get("functions_over_300_lines"), 1)
        self.assertEqual(got.get("files_over_3000_lines"), 1)


class BaselineTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "scripts").mkdir()
        write_crate(self.root, "temps-demo", {"src/lib.rs": "use anyhow::Result;\n"})

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_main(self, *args: str) -> int:
        return rule_drift.main(["--root", str(self.root), *args])

    def test_update_then_check_passes(self) -> None:
        self.assertEqual(self.run_main("--update-baseline"), 0)
        baseline = json.loads((self.root / "scripts/rule-baseline.json").read_text(encoding="utf-8"))
        self.assertEqual(baseline["crates"]["temps-demo"]["anyhow_in_library"], 1)
        self.assertEqual(self.run_main(), 0)

    def test_rise_above_baseline_fails(self) -> None:
        self.assertEqual(self.run_main("--update-baseline"), 0)
        (self.root / "crates/temps-demo/src/more.rs").write_text("fn f() -> anyhow::Result<()> { Ok(()) }\n")
        self.assertEqual(self.run_main(), 1)

    def test_new_crate_starts_at_zero(self) -> None:
        self.assertEqual(self.run_main("--update-baseline"), 0)
        write_crate(self.root, "temps-new", {"src/lib.rs": "#[derive(thiserror::Error)]\nenum E {\n    Other(String),\n}\n"})
        self.assertEqual(self.run_main(), 1)

    def test_drop_below_baseline_passes(self) -> None:
        self.assertEqual(self.run_main("--update-baseline"), 0)
        (self.root / "crates/temps-demo/src/lib.rs").write_text("pub fn clean() {}\n")
        self.assertEqual(self.run_main(), 0)

    def test_missing_baseline_is_an_error(self) -> None:
        self.assertEqual(self.run_main(), 2)


if __name__ == "__main__":
    unittest.main()
