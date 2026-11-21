// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared assertion for the language presets' unit tests: the image a preset
//! renders for a sample app runs as autopack's fixed non-root user.

use std::path::Path;

use crate::{DockerfileConfig, Preset};

/// The uid:gid autopack's runtime images run as.
pub(crate) const AUTOPACK_RUNTIME_USER: &str = "10001:10001";

/// Render `preset` for the sample app `fixture` (under `tests/fixtures`) and
/// return the last `USER` of its runtime stage.
pub(crate) async fn rendered_runtime_user(preset: &dyn Preset, fixture: &str) -> Option<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let rendered = preset
        .dockerfile(DockerfileConfig::new(&root, &root, "fixture").with_buildkit(true))
        .await;
    assert!(
        rendered.plan_failure.is_none(),
        "{} on {fixture}: {:?}",
        preset.slug(),
        rendered.plan_failure
    );
    let runtime_stage = rendered
        .content
        .rsplit("\nFROM ")
        .next()
        .unwrap_or_default();
    runtime_stage
        .lines()
        .filter_map(|line| line.trim().strip_prefix("USER "))
        .next_back()
        .map(|user| user.trim().to_string())
}
