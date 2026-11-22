<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Replace Tokenless's unmaintained YAML dependency

Please fix this in Tokenless PR #1 and return the pushed commit SHA so Temps can pin it. Do not merge.

## Problem

`tokenless-core` unconditionally depends on `serde_yaml`, which pulls in `unsafe-libyaml`. This fails Temps's existing `workspace_does_not_depend_on_unmaintained_serde_yaml` test in `crates/temps-core/src/lib.rs` and blocks PR #1849's `unit-a` CI job.

The guard permits only an existing Pingora dependency. Do not widen that exception or disable the test.

## Requested change

In Tokenless's root `Cargo.toml`, replace:

```toml
serde_yaml = "0.9"
```

with:

```toml
serde_yaml = { package = "yaml_serde", version = "0.10" }
```

The alias preserves existing `serde_yaml::` imports while selecting the maintained package. Regenerate and commit `Cargo.lock`; preserve template parsing, validation and error behavior.

## Evidence already collected

- The dependency was still present at PR #1 revision `03f8256023303cf0f0afbac9502d72cf4f8f1efc` when checked.
- The unchanged Temps guard and lockfile extracted from reviewed snapshot `c44d0857924137672b4e48eadff304505b74f892` reproduced the failure, naming `tokenless-core`.
- Applying only this manifest substitution to an isolated copy of Tokenless revision `88b66cb0dc00c5262525f9ba7d8b881066e60718` resolved `yaml_serde` 0.10.7 and passed **99 core library tests**, including template parsing and validation. This was a temporary experiment, not a published fix or validation of the full upstream workspace.

## Acceptance checks

1. Run `cargo test --lib -p tokenless-core`, plus the repository's required formatting, lint and affected-consumer checks.
2. Verify the SDK dependency graph no longer includes the actual packages `serde_yaml` or `unsafe-libyaml`; the Rust import alias is fine. Check reverse dependencies across all workspace features and targets, and account for any remaining consumers.
3. Keep or add regression coverage preventing those packages from returning through the SDK.
4. Push the fix to PR #1 and provide its immutable commit SHA and test results.

Temps will then update all Tokenless SDK pins together, regenerate its lockfile, and rerun the dependency guard and integration crate tests. No Temps security exception should change.
