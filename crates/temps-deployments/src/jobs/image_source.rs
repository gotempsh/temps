// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a deployed image lives, and therefore how a remote worker gets it.
//!
//! [`DeployImageJob`](super::DeployImageJob) has two independent questions to
//! answer about its image, and they used to share one field:
//!
//! 1. *Which tag do I deploy?* — either the output of a `BuildImageJob` in the
//!    same workflow, or a tag handed over directly (`external_image_tag`).
//! 2. *How does a remote worker obtain that tag?* — by pulling it from a
//!    registry itself, or by receiving a `docker save` stream from the control
//!    plane (`POST /agent/images/import`).
//!
//! Every uploaded image (`temps deploy:local-image`) and every
//! rollback/promotion carries a directly-handed tag, yet only images that came
//! from a registry can be pulled by a worker. Answering (2) from (1) sent
//! uploaded `temps.internal/...` images to `POST /agent/images/pull`, where the
//! worker failed to resolve the reserved `temps.internal` host.
//! [`DeployImageSource`] is the explicit answer to (2).

use serde::{Deserialize, Serialize};
use temps_entities::deployments::DeploymentMetadata;
use temps_entities::source_type::SourceType;

/// Registry host reserved for images that only ever exist in the control
/// plane's own image store (uploads and claimed daemon images). Nothing
/// resolves it, so a ref under it can never be pulled by a worker.
pub const RESERVED_LOCAL_IMAGE_PREFIX: &str = "temps.internal/";

/// Key under which the planner records [`DeployImageSource`] in a
/// `DeployImageJob` job config.
pub const IMAGE_SOURCE_CONFIG_KEY: &str = "image_source";

/// Whether `image_ref` is under the reserved, never-pullable
/// [`RESERVED_LOCAL_IMAGE_PREFIX`].
pub fn is_reserved_local_image_ref(image_ref: &str) -> bool {
    image_ref.starts_with(RESERVED_LOCAL_IMAGE_PREFIX)
}

/// How a remote worker obtains the image a deploy job ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployImageSource {
    /// The image came from a registry the worker can reach: the worker pulls
    /// it itself (`POST /agent/images/pull`).
    Registry,
    /// The image exists only in the control plane's local image store — an
    /// upload, a claimed daemon image, or an image built on the control plane.
    /// It must be exported (`docker save`) and streamed to the worker
    /// (`POST /agent/images/import`); a registry pull cannot find it.
    ControlPlaneLocal,
}

impl DeployImageSource {
    /// Stable serialized form, as stored in job configs.
    pub fn as_str(self) -> &'static str {
        match self {
            DeployImageSource::Registry => "registry",
            DeployImageSource::ControlPlaneLocal => "control_plane_local",
        }
    }

    /// Parse the serialized form. Unknown values yield `None` so the caller
    /// falls back to deriving the source, rather than guessing.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "registry" => Some(DeployImageSource::Registry),
            "control_plane_local" => Some(DeployImageSource::ControlPlaneLocal),
            _ => None,
        }
    }

    /// The source recorded in a `DeployImageJob` job config, if any. Configs
    /// written before the field existed return `None`.
    pub fn from_job_config(config: &serde_json::Value) -> Option<Self> {
        config
            .get(IMAGE_SOURCE_CONFIG_KEY)
            .and_then(|value| value.as_str())
            .and_then(Self::parse)
    }

    /// The source a deploy job must act on for `image_ref`.
    ///
    /// - A ref under [`RESERVED_LOCAL_IMAGE_PREFIX`] is always
    ///   [`ControlPlaneLocal`](Self::ControlPlaneLocal), whatever was declared:
    ///   no registry can serve it, so a pull is guaranteed to fail.
    /// - Otherwise an explicitly declared source wins.
    /// - With nothing declared (job configs written before the field existed),
    ///   a directly-handed tag keeps the historical meaning — registry — so
    ///   external-image deploys don't regress, and a tag produced by a
    ///   `BuildImageJob` in the same workflow is control-plane-local.
    pub fn resolve(image_ref: &str, declared: Option<Self>, has_external_image_tag: bool) -> Self {
        if is_reserved_local_image_ref(image_ref) {
            return DeployImageSource::ControlPlaneLocal;
        }
        match declared {
            Some(source) => source,
            None if has_external_image_tag => DeployImageSource::Registry,
            None => DeployImageSource::ControlPlaneLocal,
        }
    }

    /// Classify the image of an already-completed deployment (the origin of a
    /// rollback or promotion) from what the deployment recorded about it.
    ///
    /// - reserved `temps.internal/` ref, or an upload → control-plane-local
    /// - an external image deploy (`external_image_ref`) → registry
    /// - an explicit per-deployment source type decides next: `docker_image`
    ///   → registry, anything else (uploaded source, git, static) →
    ///   control-plane-local, whatever the project's source type
    /// - with no per-deployment evidence, a `docker_image` project → registry
    /// - everything else was built on the control plane (git, uploaded
    ///   source, manual builds) → control-plane-local. Such tags are bare
    ///   local names like `myapp-12:latest`; pulling one would ask Docker Hub
    ///   for an unrelated image of the same name.
    pub fn for_existing_deployment(
        project_source_type: SourceType,
        metadata: Option<&DeploymentMetadata>,
        image_ref: &str,
    ) -> Self {
        if is_reserved_local_image_ref(image_ref) {
            return DeployImageSource::ControlPlaneLocal;
        }
        if let Some(metadata) = metadata {
            if metadata.image_uploaded_locally {
                return DeployImageSource::ControlPlaneLocal;
            }
            if metadata
                .external_image_ref
                .as_deref()
                .is_some_and(|image| !image.is_empty())
            {
                return DeployImageSource::Registry;
            }
            // An explicit per-deployment source type overrides the project's:
            // a `docker_image` project can still carry an uploaded-source or
            // git deployment, whose image was built on the control plane.
            if let Some(deployment_source_type) = metadata.deployment_source_type {
                return if deployment_source_type == SourceType::DockerImage {
                    DeployImageSource::Registry
                } else {
                    DeployImageSource::ControlPlaneLocal
                };
            }
        }
        if project_source_type == SourceType::DockerImage {
            return DeployImageSource::Registry;
        }
        DeployImageSource::ControlPlaneLocal
    }
}

/// The image a deployment is bound to, recorded independently of its tag.
///
/// A registry tag is mutable: the control plane's copy of `app:v1` may have
/// been re-pointed since this deployment resolved it. Anything that picks up
/// "whatever the tag points at now" on the deployment's behalf must first
/// check it against this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedImageIdentity {
    /// The local image ID (`sha256:...`) resolved earlier in this deployment
    /// (e.g. by `PullExternalImageJob`).
    ImageId(String),
    /// A registry manifest digest (`sha256:...`, or `repo@sha256:...`)
    /// recorded for the deployment's registered external image.
    RepoDigest(String),
}

impl ExpectedImageIdentity {
    /// An expected image ID, or `None` for an empty/whitespace one (a pull
    /// deferred to the worker records an empty ID: nothing was resolved).
    pub fn image_id(id: &str) -> Option<Self> {
        let id = id.trim();
        (!id.is_empty()).then(|| ExpectedImageIdentity::ImageId(id.to_string()))
    }

    /// An expected registry digest, or `None` for an empty one.
    pub fn repo_digest(digest: &str) -> Option<Self> {
        let digest = digest.trim();
        (!digest.is_empty()).then(|| ExpectedImageIdentity::RepoDigest(digest.to_string()))
    }

    /// Whether `local` is the expected image. A tag match is never enough:
    /// only the ID or a registry digest counts.
    pub fn matches(&self, local: &temps_deployer::LocalImageIdentity) -> bool {
        match self {
            ExpectedImageIdentity::ImageId(expected) => {
                !local.id.is_empty() && strip_sha256(&local.id) == strip_sha256(expected)
            }
            ExpectedImageIdentity::RepoDigest(expected) => {
                let expected = digest_part(expected);
                local
                    .repo_digests
                    .iter()
                    .any(|repo_digest| digest_part(repo_digest) == expected)
            }
        }
    }
}

impl std::fmt::Display for ExpectedImageIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpectedImageIdentity::ImageId(id) => write!(f, "image ID {id}"),
            ExpectedImageIdentity::RepoDigest(digest) => write!(f, "registry digest {digest}"),
        }
    }
}

fn strip_sha256(value: &str) -> &str {
    value.trim().strip_prefix("sha256:").unwrap_or(value.trim())
}

/// The `sha256:...` part of `repo@sha256:...` (or the value itself).
fn digest_part(value: &str) -> &str {
    strip_sha256(value.rsplit('@').next().unwrap_or(value))
}

impl std::fmt::Display for DeployImageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPLOADED: &str = "temps.internal/project-1/environment-2/upload-0f3c:immutable";
    const GHCR: &str = "ghcr.io/example-org/app:v1";
    const DOCKER_HUB_LIBRARY: &str = "nginx:1.27";
    const DOCKER_HUB_QUALIFIED: &str = "docker.io/library/nginx:1.27";
    const LOCAL_GIT_BUILD: &str = "example-app-1666:latest";

    #[test]
    fn reserved_prefix_is_detected() {
        assert!(is_reserved_local_image_ref(UPLOADED));
        assert!(is_reserved_local_image_ref(
            "temps.internal/project-7/environment-11/claim-ab:immutable"
        ));
        assert!(!is_reserved_local_image_ref(GHCR));
        assert!(!is_reserved_local_image_ref(LOCAL_GIT_BUILD));
        // Only a registry-host prefix counts, not a path segment.
        assert!(!is_reserved_local_image_ref(
            "registry.example.com/temps.internal/app:v1"
        ));
    }

    #[test]
    fn serialized_form_round_trips_and_matches_serde() {
        for source in [
            DeployImageSource::Registry,
            DeployImageSource::ControlPlaneLocal,
        ] {
            assert_eq!(DeployImageSource::parse(source.as_str()), Some(source));
            assert_eq!(
                serde_json::to_value(source).unwrap(),
                serde_json::Value::String(source.as_str().to_string())
            );
        }
        assert_eq!(DeployImageSource::parse("somewhere_else"), None);
    }

    #[test]
    fn from_job_config_reads_the_recorded_source() {
        let config = serde_json::json!({ "image_source": "control_plane_local" });
        assert_eq!(
            DeployImageSource::from_job_config(&config),
            Some(DeployImageSource::ControlPlaneLocal)
        );
        let config = serde_json::json!({ "image_source": "registry" });
        assert_eq!(
            DeployImageSource::from_job_config(&config),
            Some(DeployImageSource::Registry)
        );
    }

    #[test]
    fn from_job_config_is_none_for_legacy_and_unknown_values() {
        let legacy = serde_json::json!({ "image_name": GHCR, "use_external_image": true });
        assert_eq!(DeployImageSource::from_job_config(&legacy), None);
        let unknown = serde_json::json!({ "image_source": 3 });
        assert_eq!(DeployImageSource::from_job_config(&unknown), None);
    }

    #[test]
    fn reserved_ref_is_control_plane_local_even_when_declared_registry() {
        assert_eq!(
            DeployImageSource::resolve(UPLOADED, Some(DeployImageSource::Registry), true),
            DeployImageSource::ControlPlaneLocal
        );
    }

    /// The production failure: a job config written before this field existed
    /// (`use_external_image: true`, no `image_source`) for an uploaded image.
    #[test]
    fn legacy_config_for_uploaded_image_is_control_plane_local() {
        assert_eq!(
            DeployImageSource::resolve(UPLOADED, None, true),
            DeployImageSource::ControlPlaneLocal
        );
    }

    #[test]
    fn legacy_config_for_external_image_keeps_registry() {
        for image in [GHCR, DOCKER_HUB_LIBRARY, DOCKER_HUB_QUALIFIED] {
            assert_eq!(
                DeployImageSource::resolve(image, None, true),
                DeployImageSource::Registry,
                "{image}"
            );
        }
    }

    #[test]
    fn build_job_output_without_external_tag_is_control_plane_local() {
        assert_eq!(
            DeployImageSource::resolve(LOCAL_GIT_BUILD, None, false),
            DeployImageSource::ControlPlaneLocal
        );
    }

    #[test]
    fn declared_source_wins_for_non_reserved_refs() {
        assert_eq!(
            DeployImageSource::resolve(
                LOCAL_GIT_BUILD,
                Some(DeployImageSource::ControlPlaneLocal),
                true
            ),
            DeployImageSource::ControlPlaneLocal
        );
        assert_eq!(
            DeployImageSource::resolve(GHCR, Some(DeployImageSource::Registry), true),
            DeployImageSource::Registry
        );
    }

    fn local(id: &str, repo_digests: &[&str]) -> temps_deployer::LocalImageIdentity {
        temps_deployer::LocalImageIdentity {
            id: id.to_string(),
            repo_digests: repo_digests.iter().map(|d| d.to_string()).collect(),
        }
    }

    #[test]
    fn expected_image_id_matches_only_the_same_id() {
        let expected = ExpectedImageIdentity::image_id("sha256:aaa").unwrap();
        assert!(expected.matches(&local("sha256:aaa", &[])));
        assert!(expected.matches(&local("aaa", &[])), "prefix is optional");
        assert!(!expected.matches(&local("sha256:bbb", &[])));
        assert!(!expected.matches(&local("", &[])));
    }

    #[test]
    fn expected_repo_digest_matches_any_recorded_repo_digest() {
        let expected = ExpectedImageIdentity::repo_digest("sha256:ddd").unwrap();
        assert!(expected.matches(&local(
            "sha256:aaa",
            &[
                "ghcr.io/example-org/app@sha256:eee",
                "ghcr.io/example-org/app@sha256:ddd"
            ]
        )));
        let qualified =
            ExpectedImageIdentity::repo_digest("ghcr.io/example-org/app@sha256:ddd").unwrap();
        assert!(qualified.matches(&local("sha256:aaa", &["other/app@sha256:ddd"])));
        assert!(
            !expected.matches(&local("sha256:ddd", &[])),
            "an image ID is not a digest"
        );
        assert!(!expected.matches(&local(
            "sha256:aaa",
            &["ghcr.io/example-org/app@sha256:eee"]
        )));
    }

    #[test]
    fn empty_expected_identities_are_none() {
        assert_eq!(ExpectedImageIdentity::image_id(""), None);
        assert_eq!(ExpectedImageIdentity::image_id("  "), None);
        assert_eq!(ExpectedImageIdentity::repo_digest(""), None);
    }

    fn metadata(configure: impl FnOnce(&mut DeploymentMetadata)) -> DeploymentMetadata {
        let mut metadata = DeploymentMetadata::default();
        configure(&mut metadata);
        metadata
    }

    #[test]
    fn existing_uploaded_deployment_is_control_plane_local() {
        // The upload handler records the upload as a docker_image deployment
        // with an external_image_ref, so the upload flag must win over both.
        let uploaded = metadata(|m| {
            m.external_image_ref = Some(UPLOADED.to_string());
            m.deployment_source_type = Some(SourceType::DockerImage);
            m.image_uploaded_locally = true;
        });
        assert_eq!(
            DeployImageSource::for_existing_deployment(
                SourceType::DockerImage,
                Some(&uploaded),
                UPLOADED
            ),
            DeployImageSource::ControlPlaneLocal
        );
        // Flag alone, with a ref that isn't reserved (defense in depth).
        let flagged = metadata(|m| m.image_uploaded_locally = true);
        assert_eq!(
            DeployImageSource::for_existing_deployment(
                SourceType::Manual,
                Some(&flagged),
                "uploaded:latest"
            ),
            DeployImageSource::ControlPlaneLocal
        );
    }

    #[test]
    fn existing_reserved_ref_is_control_plane_local_without_metadata() {
        assert_eq!(
            DeployImageSource::for_existing_deployment(SourceType::DockerImage, None, UPLOADED),
            DeployImageSource::ControlPlaneLocal
        );
    }

    #[test]
    fn existing_external_image_deployment_is_registry() {
        let external = metadata(|m| {
            m.external_image_ref = Some(GHCR.to_string());
            m.deployment_source_type = Some(SourceType::DockerImage);
        });
        assert_eq!(
            DeployImageSource::for_existing_deployment(SourceType::Manual, Some(&external), GHCR),
            DeployImageSource::Registry
        );
        // A docker_image project whose origin row carries no image metadata
        // (e.g. written by an older release) still pulls.
        assert_eq!(
            DeployImageSource::for_existing_deployment(
                SourceType::DockerImage,
                Some(&DeploymentMetadata::default()),
                DOCKER_HUB_LIBRARY
            ),
            DeployImageSource::Registry
        );
    }

    /// A `docker_image` project can still carry a deployment built on the
    /// control plane (uploaded source, or a per-deployment git override). Its
    /// bare local tag must never be pulled — from Docker Hub it would resolve
    /// to an unrelated image of the same name.
    #[test]
    fn explicit_non_image_deployment_source_on_docker_image_project_is_local() {
        for deployment_source in [
            SourceType::UploadedSource,
            SourceType::Git,
            SourceType::StaticFiles,
            SourceType::Manual,
        ] {
            let built_here = metadata(|m| m.deployment_source_type = Some(deployment_source));
            assert_eq!(
                DeployImageSource::for_existing_deployment(
                    SourceType::DockerImage,
                    Some(&built_here),
                    LOCAL_GIT_BUILD
                ),
                DeployImageSource::ControlPlaneLocal,
                "{deployment_source} deployment on a docker_image project"
            );
        }
        let image_deploy = metadata(|m| m.deployment_source_type = Some(SourceType::DockerImage));
        assert_eq!(
            DeployImageSource::for_existing_deployment(
                SourceType::Git,
                Some(&image_deploy),
                DOCKER_HUB_LIBRARY
            ),
            DeployImageSource::Registry,
            "a docker_image deployment on a git project is registry-sourced"
        );
    }

    #[test]
    fn existing_git_build_is_control_plane_local() {
        for project_source in [
            SourceType::Git,
            SourceType::UploadedSource,
            SourceType::Manual,
        ] {
            assert_eq!(
                DeployImageSource::for_existing_deployment(
                    project_source,
                    Some(&DeploymentMetadata::default()),
                    LOCAL_GIT_BUILD
                ),
                DeployImageSource::ControlPlaneLocal,
                "{project_source}"
            );
            assert_eq!(
                DeployImageSource::for_existing_deployment(project_source, None, LOCAL_GIT_BUILD),
                DeployImageSource::ControlPlaneLocal,
                "{project_source} without metadata"
            );
        }
    }
}
