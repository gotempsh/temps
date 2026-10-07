// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scenario tests over generic sample apps in `tests/fixtures`: detection picks
//! the right preset and the generated Dockerfile (or typed plan failure) is
//! what a deployment needs. No Docker required; the image builds live in
//! `deployment_regressions.rs`.

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
