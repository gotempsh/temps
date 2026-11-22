// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scenario tests over generic sample apps in `tests/fixtures`: detection picks
//! the right preset and the generated Dockerfile (or typed plan failure) is
//! what a deployment needs. No Docker required; the image builds live in
//! `deployment_regressions.rs`.

// Test-only helpers outside `#[test]` fns (docker/fixture wrappers): a panic
// there is the test failure, which is what we want.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use temps_presets::{
    detect_presets_from_file_tree, detect_project_candidates, AutopackPreset, BuildPlanFailure,
    DockerfileConfig, Preset, PresetType, Vite,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Relative paths and contents of every file in a fixture.
fn files(root: &Path) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(relative, std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    found
}

fn buildkit(path: &Path) -> DockerfileConfig<'_> {
    DockerfileConfig::new(path, path, "fixture").with_buildkit(true)
}

#[tokio::test]
async fn sample_vite_app_with_custom_out_dir_builds_into_nginx() {
    let root = fixture("sample-vite-custom-outdir");
    let manifest = files(&root);

    // Both detectors agree it is a static Vite site.
    let candidates = detect_project_candidates(&manifest);
    assert_eq!(candidates[0].preset, PresetType::Vite, "{candidates:?}");
    let names: Vec<String> = manifest.keys().cloned().collect();
    let detected = detect_presets_from_file_tree(&names);
    assert_eq!(detected[0].slug, "vite", "{detected:?}");

    let rendered = Vite.dockerfile(buildkit(&root)).await;
    assert!(
        rendered.plan_failure.is_none(),
        "{:?}",
        rendered.plan_failure
    );
    assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    let dockerfile = rendered.content;

    let npmrc = dockerfile
        .find(".npmrc*")
        .expect(".npmrc copied before install");
    let install = dockerfile.find("RUN npm install").expect("install step");
    let source = dockerfile.find("COPY . .").expect("source copy");
    assert!(npmrc < install && install < source, "{dockerfile}");
    assert!(
        dockerfile.contains("COPY --from=builder /app/build /usr/share/nginx/html"),
        "vite.config.ts sets build.outDir to 'build': {dockerfile}"
    );
    assert!(!dockerfile.contains("/app/dist"), "{dockerfile}");
}

#[tokio::test]
async fn sample_vite_app_without_build_script_fails_planning() {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in files(&fixture("sample-vite-custom-outdir")) {
        let contents = if path == "package.json" {
            contents.replace("\"build\": \"vite build\"", "\"preview\": \"vite preview\"")
        } else {
            contents
        };
        std::fs::write(dir.path().join(path), contents).unwrap();
    }
    let rendered = Vite.dockerfile(buildkit(dir.path())).await;
    let Some(failure @ BuildPlanFailure::MissingBuildScript { .. }) = rendered.plan_failure else {
        panic!(
            "expected MissingBuildScript, got {:?}",
            rendered.plan_failure
        );
    };
    assert!(failure.to_string().contains("Missing script: build"));
}

#[tokio::test]
async fn sample_flask_app_renders_an_autopack_dockerfile() {
    let root = fixture("sample-flask-app");
    let rendered = AutopackPreset::new().dockerfile(buildkit(&root)).await;
    assert!(
        rendered.plan_failure.is_none(),
        "{:?}",
        rendered.plan_failure
    );
    assert!(
        rendered.content.starts_with("# syntax="),
        "{}",
        rendered.content
    );
    // Listens on the PORT Temps injects, defaulting to the framework port.
    assert!(
        rendered
            .content
            .contains("gunicorn app:app --bind 0.0.0.0:${PORT:-8000}"),
        "{}",
        rendered.content
    );
}

#[tokio::test]
async fn python_app_without_an_entry_point_fails_fast_with_a_typed_outcome() {
    let root = fixture("sample-python-no-entrypoint");
    let rendered = AutopackPreset::new().dockerfile(buildkit(&root)).await;
    let Some(BuildPlanFailure::Unplannable { preset, reason }) = rendered.plan_failure.clone()
    else {
        panic!("expected Unplannable, got {:?}", rendered.plan_failure);
    };
    assert_eq!(preset, "autopack");
    assert!(!reason.is_empty());
    // Callers that only read the content still get a build that fails loudly.
    assert!(rendered.content.contains("exit 1"), "{}", rendered.content);
    assert!(rendered
        .plan_failure
        .unwrap()
        .to_string()
        .contains("could not plan this application"));
}

/// The final stage of a rendered Dockerfile: everything after its last `FROM`.
fn final_stage(dockerfile: &str) -> &str {
    let start = dockerfile
        .match_indices("\nFROM ")
        .last()
        .map(|(index, _)| index + 1)
        .unwrap_or(0);
    &dockerfile[start..]
}

/// The user the image runs as: the last `USER` of the final stage.
fn runtime_user(dockerfile: &str) -> Option<&str> {
    final_stage(dockerfile)
        .lines()
        .filter_map(|line| line.trim().strip_prefix("USER "))
        .next_back()
        .map(str::trim)
}

/// Every server-language preset built by autopack, with a sample app it
/// detects. Static presets are not here: Temps serves their files, it never
/// runs their image.
const NON_ROOT_PRESETS: &[(&str, &str)] = &[
    ("python", "sample-flask-app"),
    ("go", "sample-go-app"),
    ("rust", "sample-rust-app"),
    ("java", "sample-java-app"),
    ("autopack", "sample-flask-app"),
    ("nixpacks", "sample-go-app"),
    ("nixpacks-python", "sample-flask-app"),
    ("nixpacks-go", "sample-go-app"),
    ("nixpacks-rust", "sample-rust-app"),
    ("nixpacks-java", "sample-java-app"),
];

/// The uid:gid every autopack runtime image runs as.
const RUNTIME_USER: &str = "10001:10001";

#[tokio::test]
async fn server_presets_run_as_a_fixed_non_root_user() {
    for (slug, app) in NON_ROOT_PRESETS {
        let root = fixture(app);
        let preset = temps_presets::get_preset_by_slug(slug).unwrap();
        let rendered = preset.dockerfile(buildkit(&root)).await;
        assert!(
            rendered.plan_failure.is_none(),
            "{slug} on {app}: {:?}",
            rendered.plan_failure
        );
        let stage = final_stage(&rendered.content);
        assert_eq!(
            runtime_user(&rendered.content),
            Some(RUNTIME_USER),
            "{slug}:\n{stage}"
        );

        // `USER` comes after every `RUN` of the runtime stage, so nothing the
        // image does at build time is left for the app to need root for.
        let lines: Vec<&str> = stage.lines().map(str::trim).collect();
        let user_at = lines
            .iter()
            .rposition(|line| line.starts_with("USER "))
            .unwrap();
        assert!(
            lines[user_at..]
                .iter()
                .all(|line| !line.starts_with("RUN ")),
            "{slug}: a RUN follows USER:\n{stage}"
        );
        // Only what the app writes is handed to it: the working directory
        // and the files copied in, never a recursive chown of the image.
        assert!(
            lines[..user_at].contains(&"RUN chown 10001:10001 /app"),
            "{slug}: /app is not writable by the runtime user:\n{stage}"
        );
        assert!(!stage.contains("chown -R"), "{slug}:\n{stage}");
        assert!(
            stage.contains("COPY --chown=10001:10001 "),
            "{slug}: app files are not owned by the runtime user:\n{stage}"
        );
        assert!(
            lines[user_at..].contains(&"ENV HOME=/home/autopack"),
            "{slug}: HOME is not the runtime user's:\n{stage}"
        );
    }
}

#[tokio::test]
async fn autopack_user_root_is_an_explicit_opt_out_to_root() {
    let root = fixture("sample-go-app");
    let vars = vec!["AUTOPACK_USER=root".to_string()];
    let rendered = AutopackPreset::new()
        .dockerfile(buildkit(&root).with_build_vars(&vars))
        .await;
    assert!(
        rendered.plan_failure.is_none(),
        "{:?}",
        rendered.plan_failure
    );
    // No USER at all: the image runs as root, which the deploy pipeline's
    // root-user warning reports.
    assert_eq!(
        runtime_user(&rendered.content),
        None,
        "{}",
        rendered.content
    );
}
