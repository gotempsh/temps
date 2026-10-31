---
name: temps-split-module
description: Mechanically split a large Rust source file (a "god file") into a module directory with no behaviour change - move-only commits, pub use re-exports that keep the public API, impl blocks spread across submodules, narrow pub(super)/pub(crate) visibility, a sibling tests.rs for oversized inline tests, and the rule-drift and test-count checks that prove nothing changed. Use when a file is over ~3,000 production lines, when inline tests push it past ~1,500 lines, or when asked to split or reorganise a module.
---
<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Split a large module

Rust lets one type have several `impl` blocks in different modules of the same
crate, so most god files can be split by **moving code only**: no renamed
functions, no changed signatures, no "while I'm here" fixes. A move-only diff is
cheap to review; a mixed one is not.

Context: `docs/plans/crates-refactor-plan.md` §4 P2 lists the target files and
proposed modules; §7 decisions 1 and 2 set the test-file convention and the size
limits. Typical god files: `crates/temps-providers/src/services.rs`,
`crates/temps-deployer/src/docker.rs`.

## Rules

- **One file per PR**, and nothing else in that PR. Fixes found while moving go
  in a later PR (note them in the PR description).
- **No behaviour change.** Same tests, same test count, same public paths.
- **Keep the public API.** Every path other code uses (`crate::services::Foo`,
  `temps_x::services::Foo`) still resolves, via `pub use`.
- **Narrowest visibility that compiles.** Never widen to `pub` to make a move work.
- **Split by responsibility**, along seams that already exist: separate trait
  impls, groups of methods that call each other, types and errors, helpers.

## Target layout

Either form is used in this repo; follow the crate's existing style.

```text
foo/mod.rs            # module docs, shared imports, types, pub use re-exports
foo/lifecycle.rs      # impl Foo { ... } - one responsibility
foo/restore.rs        # impl Foo { ... }
foo/tests.rs          # #[cfg(test)] mod tests; when the tests are large
```

Real examples:

- `crates/temps-error-tracking/src/services/source_map_service.rs` with
  `source_map_service/{artifacts,build_credentials,debug_ids}.rs`:
  `mod debug_ids;` plus `pub use artifacts::{ArtifactAssembly, ...};` in the
  parent, and `debug_ids.rs` holding `impl SourceMapService { pub(super) async fn
  symbolicate_debug_ids(..) }`.
- `crates/temps-monitoring/src/http_checks/mod.rs` with `automatic.rs`,
  `secret_history.rs`, `transport.rs`, `variable_history.rs`: `automatic.rs`,
  `secret_history.rs` and `variable_history.rs` start with `use super::*;` and
  add an `impl HttpChecksService` block, sharing `pub(super)` items.
- Out-of-line tests: `crates/temps-status-page/src/lib.rs` declares
  `#[cfg(test)] mod tests;` and the tests live in `crates/temps-status-page/src/tests.rs`.

## Procedure

### 0. Record the baseline

```bash
cargo test --lib -p <crate> -- --list 2>/dev/null | grep -c ': test$'
python3 scripts/rule_drift.py --list files_over_3000_lines | grep <file>
python3 scripts/rule_drift.py --list functions_over_300_lines | grep <file>
```

Keep the test count; it must be identical at the end.

### 1. Turn the file into a directory (history-preserving)

```bash
git mv crates/<crate>/src/foo.rs crates/<crate>/src/foo/mod.rs
```

Commit this rename on its own (`refactor(<crate>): move foo.rs to foo/mod.rs`)
so git keeps the file's history and blame. Skip it if the crate uses the
`foo.rs` + `foo/` form.

### 2. Move tests out first (decision 1)

If inline tests push the file past ~1,500 lines, move the whole
`#[cfg(test)] mod tests { ... }` body into `foo/tests.rs` and leave:

```rust
#[cfg(test)]
mod tests;
```

`tests.rs` starts with the SPDX header and `use super::*;`. Being a child of
`foo`, it still sees `foo`'s private items. Tests stay beside the code, never in a
distant directory. When a later step moves code into `foo/bar.rs`, tests that
exercise only `bar` may move into `bar.rs`'s own `mod tests`.

### 3. Extract one submodule at a time

For each responsibility:

1. Create `foo/<name>.rs` with the SPDX header
   (`python3 scripts/source_attribution.py annotate <file>`) and `use super::*;`
   or explicit `use super::{..}` imports.
2. Cut the items **verbatim** (doc comments and attributes included) and paste
   them. For methods, wrap them in `impl Type { ... }` in the new file.
3. Declare it in `mod.rs`: `mod <name>;` and, for anything callers outside
   `foo` use, `pub use <name>::{Item, ..};`.
4. Fix visibility only as far as the compiler asks:
   - An item that was private in `foo` and is now in `foo::bar` but used by
     `foo` or a sibling becomes `pub(super)`.
   - `pub(crate)` only if something outside `foo` in the same crate uses it.
   - Items that were `pub` stay `pub` and are re-exported.
   - Private items left in `mod.rs` are already visible to its children.
5. `cargo check --lib -p <crate>` and commit:
   `refactor(<crate>): move <responsibility> into foo/<name>.rs`.

Small commits make `git diff --color-moved=dimmed-zebra` show almost every line
as moved; review that before pushing.

### 4. Keep the rule-drift counts flat

`scripts/rule_drift.py` counts some metrics **per line**, so a move can raise a
count without changing behaviour:

- `anyhow_in_library` counts every line that mentions `anyhow`. Repeating
  `use anyhow::Result;` in each new submodule raises it. Import through the
  parent (`use super::*;`) instead.
- `handler_db_access` counts `use sea_orm` lines in any path containing
  `/handlers/` or ending in `handlers.rs`. Moving service code under a
  `handlers/` directory, or duplicating `sea_orm` imports there, raises it.
- `files_over_3000_lines` counts non-blank production lines after removing
  `#[cfg(test)]` items, and an out-of-line `#[cfg(test)] mod x;` file is not
  scanned. So moving tests out does not change it; splitting production code does.
- `functions_over_300_lines` does not change on a move. Splitting a long
  function is a redesign (P2's R-items), not part of this skill.

When counts fall, lower the baseline in the same PR:

```bash
python3 scripts/rule_drift.py --update-baseline
git diff scripts/rule-baseline.json   # only decreases, only for this crate
```

A rise means the split duplicated something; fix the imports rather than
raising the baseline.

### 5. Check callers outside the crate

Grep the workspace for the old paths (`temps_<crate>::foo::`) and run
`cargo check --lib -p <dependent>` for each dependent crate. A missing
`pub use` shows up here, not in the crate itself.

## Verification

```bash
cargo fmt -p <crate>
cargo check --lib -p <crate>
cargo test --lib -p <crate>
cargo test --lib -p <crate> -- --list 2>/dev/null | grep -c ': test$'   # same as step 0
cargo clippy -p <crate> --all-targets -- -D warnings
python3 scripts/rule_drift.py
python3 scripts/source_attribution.py check
git diff --stat origin/<base>...HEAD     # only this module's files and the baseline
```

Tests that need Docker skip at runtime when it is unavailable; run them with
Docker available if the moved code is Docker- or database-backed, since a skip
proves nothing.

## PR

- Title: `refactor(<crate>): split <file> into modules`.
- Body: the module map (file → responsibility), "move-only, no behaviour change",
  test count before and after, rule-drift counts before and after, and any issues
  noticed but deliberately not fixed.
- Every commit uses Conventional Commits and `git commit -s`.

## Checklist

- [ ] One source file split; nothing else changed
- [ ] Rename committed separately with `git mv`
- [ ] Every moved item byte-identical apart from visibility
- [ ] Public paths preserved with `pub use`; dependents compile
- [ ] Visibility no wider than needed (`pub(super)` before `pub(crate)`)
- [ ] Large tests in a sibling `tests.rs` declared `#[cfg(test)] mod tests;`
- [ ] New files carry the SPDX header
- [ ] Same test count; clippy clean; rule-drift flat or lower, baseline updated
