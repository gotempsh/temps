// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The build job's side of identical build coordination (ADR 113).
//!
//! For a project whose `identical_build_mode` is `queue` or `share`, the
//! build job identifies its build ([`identify`]), then asks the coordinator
//! whether it may build now, must wait, or can deploy the image of an
//! identical share build instead. Waiting happens here, before the build is
//! offered to a worker, so a waiting build holds no build slot. Every step
//! is written to the build log in words the reader can act on, and a build
//! in another access boundary is never named.

use super::*;
use crate::services::identical_builds::{
    identify, Admission, AdmissionEvent, AdmissionObserver, BuildInputs, BuildTicket, BuilderKind,
    DockerfileSource, FingerprintKey, IdenticalBuildCoordinator, IdenticalBuildError, LeaderView,
    ReusedBuild, SharedImage, SourceIdentity,
};
use temps_entities::identical_build_mode::{
    IdenticalBuildMode, IdenticalBuildScope, IdenticalBuildState,
};

/// Build job output naming the deployment whose image this deployment
/// reused instead of building. Absent for a deployment that built its own.
pub const REUSED_IMAGE_FROM_DEPLOYMENT_OUTPUT: &str = "reused_image_from_deployment_id";

/// Largest Dockerfile read to identify a build.
const MAX_DOCKERFILE_BYTES: u64 = 5 * 1024 * 1024;
/// Largest `.temps.yaml` read to identify a build.
const MAX_TEMPS_YAML_BYTES: u64 = 1024 * 1024;

/// How this project's builds coordinate, and what identifies them beyond
/// the job's own configuration.
#[derive(Clone)]
pub struct IdenticalBuildSettings {
    pub coordinator: IdenticalBuildCoordinator,
    /// Keys the fingerprint (the install's encryption service).
    pub key: Arc<dyn FingerprintKey>,
    pub mode: IdenticalBuildMode,
    pub scope: IdenticalBuildScope,
    pub project_id: i32,
    pub source: SourceIdentity,
    /// See `services::identical_builds::access_boundary`.
    pub access_boundary: String,
}

impl std::fmt::Debug for IdenticalBuildSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdenticalBuildSettings")
            .field("mode", &self.mode)
            .field("scope", &self.scope)
            .field("project_id", &self.project_id)
            .field("source", &self.source)
            .field("access_boundary", &self.access_boundary)
            .finish()
    }
}

/// How a build the log talks about is named to its reader.
fn leader_name(leader: &LeaderView) -> String {
    match leader.deployment_id {
        Some(id) if leader.same_project => format!("deployment #{id} of this project"),
        Some(id) => format!("deployment #{id}"),
        // Another access boundary: whoever reads this log may not see it.
        None => "a build of another project".to_string(),
    }
}

/// The build-log line for an admission event.
pub(crate) fn describe_event(event: &AdmissionEvent) -> String {
    match event {
        AdmissionEvent::Waiting {
            leader: Some(leader),
            ..
        } if leader.will_share => format!(
            "⏳ Waiting for an identical build ({}). This deployment will reuse its image \
             instead of building again",
            leader_name(leader)
        ),
        AdmissionEvent::Waiting {
            leader: Some(leader),
            ..
        } => format!(
            "⏳ Waiting for {} to finish building the same source: identical builds run one \
             at a time. This deployment builds its own image next",
            leader_name(leader)
        ),
        AdmissionEvent::Waiting {
            leader: None,
            ahead,
        } => format!(
            "⏳ Waiting in line behind {ahead} identical build{}: identical builds run one at a time",
            if *ahead == 1 { "" } else { "s" }
        ),
        AdmissionEvent::LeaderEnded { leader, state } => {
            let ended = match state {
                IdenticalBuildState::Failed => "failed",
                IdenticalBuildState::Abandoned => "stopped responding",
                IdenticalBuildState::TimedOut => "timed out",
                _ => "was cancelled",
            };
            format!(
                "The build this deployment was waiting for ({}) {ended}; this deployment builds \
                 the image itself",
                leader_name(leader)
            )
        }
        AdmissionEvent::ReuseUnavailable {
            deployment_id,
            reason,
        } => format!(
            "The image of an identical build (deployment #{deployment_id}) cannot be reused: \
             {reason}. This deployment does not depend on it"
        ),
    }
}

/// The build-log line for a reused image.
pub(crate) fn describe_reuse(reused: &ReusedBuild) -> String {
    let short_sha: String = reused.commit_sha.chars().take(7).collect();
    let origin = if reused.same_project {
        format!("deployment #{} of this project", reused.deployment_id)
    } else {
        format!("deployment #{}", reused.deployment_id)
    };
    format!(
        "♻️ Reused image from {origin}, built from {}@{short_sha}: an identical build (same \
         repository, commit, build path and build-time values) already produced it, so nothing \
         was built",
        reused.repository
    )
}

/// Logs admission events to the build log.
struct BuildLogObserver<'a> {
    job: &'a BuildImageJob,
    context: &'a WorkflowContext,
}

#[async_trait]
impl AdmissionObserver for BuildLogObserver<'_> {
    async fn observe(&self, event: AdmissionEvent) {
        if let Err(error) = self.job.log(self.context, describe_event(&event)).await {
            tracing::warn!(
                deployment_id = self.context.deployment_id,
                %error,
                "Could not write an identical build event to the build log"
            );
        }
    }

    async fn control_plane_image_missing(&self, image: &SharedImage) -> Option<String> {
        let mut tags: Vec<(&str, Option<&str>)> = vec![(&image.image_tag, Some(&image.image_id))];
        for (platform, tag) in &image.image_tags_by_platform {
            tags.push((
                tag,
                image
                    .image_ids_by_platform
                    .get(platform)
                    .map(String::as_str),
            ));
        }
        for (tag, expected) in tags {
            match self.job.image_builder.inspect_image(tag).await {
                Ok(info) => {
                    if let Some(expected) = expected {
                        if !super::super::node_image::same_image_id(&info.id, expected) {
                            return Some(format!(
                                "the control plane's '{tag}' is now a different image"
                            ));
                        }
                    }
                }
                Err(error) => {
                    return Some(format!(
                        "the control plane no longer holds '{tag}' ({error})"
                    ));
                }
            }
        }
        None
    }
}

/// Whether `path` is relative and stays inside the directory it is joined to.
fn is_contained(path: &str) -> bool {
    let path = Path::new(path);
    !path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
}

impl BuildImageJob {
    /// Coordinate this build with identical builds, if its project asked for
    /// it. Anything that prevents identifying the build makes it build on its
    /// own, as every build did before, and the log says why.
    pub(super) async fn execute_with_identical_builds(
        &self,
        context: WorkflowContext,
    ) -> Result<JobResult, WorkflowError> {
        let Some(settings) = &self.identical_builds else {
            return self.execute_build(context).await;
        };
        let ticket = match self.identical_build_ticket(settings, &context) {
            Ok(ticket) => ticket,
            Err(reason) => {
                self.log(
                    &context,
                    format!(
                        "Identical builds ({}): this build runs on its own because {reason}",
                        settings.mode
                    ),
                )
                .await?;
                return self.execute_build(context).await;
            }
        };
        self.log(
            &context,
            format!(
                "Identical builds ({}, grouped by {}): checking for an identical build of {}@{} \
                 ({} build-time value{} in the fingerprint)",
                settings.mode,
                settings.scope,
                ticket.repository,
                ticket.commit_sha.chars().take(7).collect::<String>(),
                ticket.identity.considered_values,
                if ticket.identity.considered_values == 1 {
                    ""
                } else {
                    "s"
                },
            ),
        )
        .await?;

        let observer = BuildLogObserver {
            job: self,
            context: &context,
        };
        match settings.coordinator.admit(&ticket, &observer).await {
            Ok(Admission::Reuse(reused)) => self.deploy_reused_image(context, &reused).await,
            Ok(Admission::Build(lease)) => {
                let result = self.execute_build(context).await;
                let outcome = match &result {
                    Ok(job_result) => {
                        let built_where_identified = built_on(&job_result.context, &self.job_id)
                            == self.remote_builder_node_id;
                        if !built_where_identified {
                            // The worker stayed busy or refused, and the
                            // control plane built it with other inputs (every
                            // value, its own platform): not what the
                            // fingerprint describes, so not offered.
                            tracing::info!(
                                deployment_id = ticket.deployment_id,
                                "Identical build ran on another builder than it was identified for; its image is not offered for reuse"
                            );
                        }
                        let image = if settings.mode.shares_images() && built_where_identified {
                            match shared_image(
                                &job_result.context,
                                &self.job_id,
                                &self.download_job_id,
                            ) {
                                Ok(image) => Some(image),
                                Err(error) => {
                                    tracing::warn!(
                                        deployment_id = ticket.deployment_id,
                                        %error,
                                        "Built image cannot be described for reuse; identical builds will build their own"
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        lease.succeed(image.as_ref()).await
                    }
                    // The error text can carry build output; the row only
                    // says where to look.
                    Err(_) => {
                        lease
                            .fail(&format!(
                                "the build failed; see the build log of deployment {}",
                                ticket.deployment_id
                            ))
                            .await
                    }
                };
                if let Err(error) = outcome {
                    // The build itself is done; waiting builds take over once
                    // the row's heartbeat is stale.
                    tracing::warn!(
                        deployment_id = ticket.deployment_id,
                        %error,
                        "Could not record the outcome of an identical build"
                    );
                }
                result
            }
            Err(error @ IdenticalBuildError::WaitTimedOut { .. }) => {
                let message = error.to_string();
                self.log(&context, format!("ERROR: {message}")).await?;
                Err(WorkflowError::JobExecutionFailed(message))
            }
            Err(error) => {
                // Degrade, don't fail the deployment: coordination is an
                // optimisation, the build is what was asked for.
                tracing::warn!(
                    deployment_id = ticket.deployment_id,
                    %error,
                    "Identical build coordination unavailable; building on its own"
                );
                self.log(
                    &context,
                    format!(
                        "⚠️ Could not coordinate with identical builds ({error}); building on its own"
                    ),
                )
                .await?;
                self.execute_build(context).await
            }
        }
    }

    /// What identifies this build, or why it cannot be identified.
    fn identical_build_ticket(
        &self,
        settings: &IdenticalBuildSettings,
        context: &WorkflowContext,
    ) -> Result<BuildTicket, String> {
        let repo = RepositoryOutput::from_context(context, &self.download_job_id)
            .map_err(|error| format!("the checkout could not be read ({error})"))?;
        let commit_sha: String = context
            .get_output(&self.download_job_id, "commit_sha")
            .ok()
            .flatten()
            .ok_or_else(|| "the checked-out commit is unknown".to_string())?;
        let build_context = self
            .build_config
            .build_context
            .clone()
            .unwrap_or_else(|| ".".to_string());
        let dockerfile_path = self
            .build_config
            .dockerfile_path
            .clone()
            .unwrap_or_else(|| "Dockerfile".to_string());
        if !is_contained(&build_context) || !is_contained(&dockerfile_path) {
            return Err("its build context or Dockerfile path leaves the checkout".to_string());
        }
        let context_dir = repo.repo_dir.join(&build_context);
        let unreadable =
            |what: &str, error: WorkflowError| format!("its {what} could not be read ({error})");
        let dockerfile = read_confined_control_file(
            &context_dir,
            &context_dir.join(&dockerfile_path),
            MAX_DOCKERFILE_BYTES,
        )
        .map_err(|error| unreadable("Dockerfile", error))?;
        // Both `.temps.yaml` files a build reads: the build context's and the
        // repository root's.
        let mut temps_yaml = Vec::new();
        for (label, root) in [("context", &context_dir), ("root", &repo.repo_dir)] {
            if let Some(contents) =
                read_confined_control_file(root, &root.join(".temps.yaml"), MAX_TEMPS_YAML_BYTES)
                    .map_err(|error| unreadable(".temps.yaml", error))?
            {
                temps_yaml.extend_from_slice(label.as_bytes());
                temps_yaml.extend_from_slice(&(contents.len() as u64).to_be_bytes());
                temps_yaml.extend_from_slice(contents.as_bytes());
            }
        }
        let preset = self
            .preset
            .map(|preset| serde_json::to_string(&preset))
            .transpose()
            .map_err(|error| format!("its preset could not be encoded ({error})"))?;
        let preset_config = self
            .preset_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| format!("its preset configuration could not be encoded ({error})"))?;
        let platforms: Vec<String> = if !self.platform_builders.is_empty() {
            self.platform_builders
                .iter()
                .map(|builder| builder.platform.clone())
                .collect()
        } else if !self.build_config.target_platforms.is_empty() {
            self.build_config.target_platforms.clone()
        } else {
            vec![self.image_builder.get_native_platform()]
        };
        let builder = if self.external_build {
            BuilderKind::Ci
        } else if self.remote_builder_node_id.is_some() {
            BuilderKind::Worker
        } else {
            BuilderKind::ControlPlane
        };
        let inputs = BuildInputs {
            source: &settings.source,
            commit_sha: &commit_sha,
            build_context: &build_context,
            dockerfile_path: &dockerfile_path,
            dockerfile: match &dockerfile {
                Some(contents) => DockerfileSource::Repository { contents },
                None => DockerfileSource::Generated,
            },
            preset: preset.as_deref(),
            preset_config: preset_config.as_deref(),
            temps_yaml: (!temps_yaml.is_empty()).then_some(temps_yaml.as_slice()),
            platforms: &platforms,
            builder,
            registry_mirror_prefix: self.registry_mirror_prefix.as_deref(),
            cache_from: &self.build_config.cache_from,
            build_args: &self.build_config.build_args,
            buildkit_build_args: &self.build_config.build_args_buildkit,
            public_names: &self.worker_build_arg_keys,
        };
        let identity =
            identify(&inputs, settings.key.as_ref()).map_err(|error| error.to_string())?;
        Ok(BuildTicket {
            deployment_id: context.deployment_id,
            project_id: settings.project_id,
            job_id: self.job_id.clone(),
            mode: settings.mode,
            scope: settings.scope,
            identity,
            access_boundary: settings.access_boundary.clone(),
            repository: settings.source.repository(),
            commit_sha: commit_sha.to_ascii_lowercase(),
        })
    }

    /// Finish this job with the image of `reused` instead of building: the
    /// same outputs a build records, so the deploy job hands the image to
    /// its nodes from wherever it lives.
    async fn deploy_reused_image(
        &self,
        mut context: WorkflowContext,
        reused: &ReusedBuild,
    ) -> Result<JobResult, WorkflowError> {
        self.log(&context, describe_reuse(reused)).await?;
        let repo = RepositoryOutput::from_context(&context, &self.download_job_id)?;
        let image = &reused.image;
        // Where the identical build actually built from (a workspace build
        // widens the context), in this deployment's own checkout.
        let configured = self.build_config.build_context.as_deref().unwrap_or(".");
        let build_root = image
            .build_root
            .as_deref()
            .filter(|root| is_contained(root))
            .unwrap_or(configured);
        let build_context = repo.repo_dir.join(build_root);
        let dockerfile_path = match image.dockerfile_path.as_deref().filter(|p| is_contained(p)) {
            Some(path) => repo.repo_dir.join(path),
            None => repo.repo_dir.join(configured).join(
                self.build_config
                    .dockerfile_path
                    .as_deref()
                    .unwrap_or("Dockerfile"),
            ),
        };
        let output = ImageOutput {
            image_tag: image.image_tag.clone(),
            image_id: image.image_id.clone(),
            size_bytes: image.size_bytes,
            build_context,
            dockerfile_path,
            image_tags_by_platform: image.image_tags_by_platform.clone(),
            builder_node_ids_by_platform: image.builder_node_ids_by_platform.clone(),
            image_ids_by_platform: image.image_ids_by_platform.clone(),
            image_config: image.image_config.clone(),
            dockerfile_preset: image.dockerfile_preset.clone(),
        };
        self.record_image_outputs(&mut context, &output, image.builder_node_id)?;
        context.set_output(
            &self.job_id,
            REUSED_IMAGE_FROM_DEPLOYMENT_OUTPUT,
            reused.deployment_id,
        )?;
        Ok(JobResult::success(context))
    }
}

/// The worker node a finished build recorded as holding its image; `None`
/// when the control plane built it.
fn built_on(context: &WorkflowContext, job_id: &str) -> Option<i32> {
    context
        .get_output::<Option<i32>>(job_id, "builder_node_id")
        .ok()
        .flatten()
        .flatten()
}

/// `path` relative to `root`, as a contained `/`-separated path, when it is
/// inside it.
fn checkout_relative(root: &Path, path: &Path) -> Option<String> {
    let relative: PathBuf = match path.strip_prefix(root) {
        Ok(relative) => relative.to_path_buf(),
        // One side may be spelled through a symlink (`/tmp` and
        // `/private/tmp`).
        Err(_) => {
            let root = root.canonicalize().ok()?;
            let path = path.canonicalize().ok()?;
            path.strip_prefix(&root).ok()?.to_path_buf()
        }
    };
    let relative = relative.to_str()?.trim_start_matches('/');
    let relative = if relative.is_empty() { "." } else { relative };
    is_contained(relative).then(|| relative.to_string())
}

/// The image a successful build recorded, as offered to identical builds.
fn shared_image(
    context: &WorkflowContext,
    job_id: &str,
    download_job_id: &str,
) -> Result<SharedImage, WorkflowError> {
    let output = ImageOutput::from_context(context, job_id)?;
    let builder_node_id = built_on(context, job_id);
    let checkout = RepositoryOutput::from_context(context, download_job_id)
        .ok()
        .map(|repo| repo.repo_dir);
    let relative = |path: &Path| {
        checkout
            .as_deref()
            .and_then(|root| checkout_relative(root, path))
    };
    Ok(SharedImage {
        build_root: relative(&output.build_context),
        dockerfile_path: relative(&output.dockerfile_path),
        image_tag: output.image_tag,
        image_id: output.image_id,
        size_bytes: output.size_bytes,
        builder_node_id,
        image_tags_by_platform: output.image_tags_by_platform,
        builder_node_ids_by_platform: output.builder_node_ids_by_platform,
        image_ids_by_platform: output.image_ids_by_platform,
        image_config: output.image_config,
        dockerfile_preset: output.dockerfile_preset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leader(deployment_id: Option<i32>, same_project: bool, will_share: bool) -> LeaderView {
        LeaderView {
            build_id: 1,
            deployment_id,
            same_project,
            will_share,
        }
    }

    #[test]
    fn a_waiting_share_build_says_it_will_reuse_the_image() {
        let line = describe_event(&AdmissionEvent::Waiting {
            leader: Some(leader(Some(41), false, true)),
            ahead: 0,
        });
        assert!(
            line.contains("Waiting for an identical build (deployment #41)"),
            "{line}"
        );
        assert!(line.contains("reuse its image"), "{line}");
    }

    #[test]
    fn a_queued_build_says_it_builds_next() {
        let line = describe_event(&AdmissionEvent::Waiting {
            leader: Some(leader(Some(41), true, false)),
            ahead: 0,
        });
        assert!(line.contains("deployment #41 of this project"), "{line}");
        assert!(line.contains("builds its own image next"), "{line}");
        let line = describe_event(&AdmissionEvent::Waiting {
            leader: None,
            ahead: 2,
        });
        assert!(line.contains("behind 2 identical builds"), "{line}");
    }

    #[test]
    fn a_build_in_another_access_boundary_is_never_named() {
        for event in [
            AdmissionEvent::Waiting {
                leader: Some(leader(None, false, false)),
                ahead: 0,
            },
            AdmissionEvent::LeaderEnded {
                leader: leader(None, false, false),
                state: IdenticalBuildState::Failed,
            },
        ] {
            let line = describe_event(&event);
            assert!(line.contains("a build of another project"), "{line}");
            assert!(!line.contains('#'), "{line}");
        }
    }

    #[test]
    fn a_take_over_says_why() {
        for (state, words) in [
            (IdenticalBuildState::Failed, "failed"),
            (IdenticalBuildState::Cancelled, "was cancelled"),
            (IdenticalBuildState::Abandoned, "stopped responding"),
        ] {
            let line = describe_event(&AdmissionEvent::LeaderEnded {
                leader: leader(Some(7), false, true),
                state,
            });
            assert!(line.contains(words), "{line}");
            assert!(line.contains("builds the image itself"), "{line}");
        }
    }

    #[test]
    fn a_reuse_names_the_build_repository_and_commit() {
        let reused = ReusedBuild {
            build_id: 3,
            deployment_id: 41,
            same_project: false,
            image: SharedImage {
                image_tag: "a:latest".into(),
                image_id: "sha256:1".into(),
                size_bytes: 1,
                builder_node_id: None,
                image_tags_by_platform: Default::default(),
                builder_node_ids_by_platform: Default::default(),
                image_ids_by_platform: Default::default(),
                image_config: None,
                dockerfile_preset: None,
                build_root: None,
                dockerfile_path: None,
            },
            repository: "acme/shop".into(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        };
        let line = describe_reuse(&reused);
        assert!(
            line.contains("Reused image from deployment #41, built from acme/shop@0123456"),
            "{line}"
        );
    }

    #[test]
    fn an_unreachable_image_says_why_and_names_its_build() {
        let line = describe_event(&AdmissionEvent::ReuseUnavailable {
            deployment_id: 41,
            reason: "the build node holding it (3) is not active".into(),
        });
        assert!(line.contains("deployment #41"), "{line}");
        assert!(line.contains("(3) is not active"), "{line}");
        assert!(line.contains("does not depend on it"), "{line}");
    }

    #[test]
    fn the_build_root_is_recorded_relative_to_the_checkout() {
        let checkout = Path::new("/work/deployment-4/repository");
        // A workspace build widened `apps/web` to the repository root.
        assert_eq!(checkout_relative(checkout, checkout).as_deref(), Some("."));
        assert_eq!(
            checkout_relative(checkout, &checkout.join("apps/web")).as_deref(),
            Some("apps/web")
        );
        assert_eq!(
            checkout_relative(checkout, &checkout.join("apps/web/Dockerfile")).as_deref(),
            Some("apps/web/Dockerfile")
        );
        // Outside the checkout: not recorded, the follower uses its own.
        assert_eq!(
            checkout_relative(checkout, Path::new("/tmp/elsewhere")),
            None
        );
    }

    #[test]
    fn only_a_build_that_ran_where_it_was_identified_is_offered() {
        let mut context =
            crate::test_utils::create_test_context("wf-identical".to_string(), 1, 1, 1);
        // A control-plane fallback records no builder node.
        context
            .set_output("build_image", "builder_node_id", Option::<i32>::None)
            .unwrap();
        assert_eq!(built_on(&context, "build_image"), None);
        context
            .set_output("build_image", "builder_node_id", Some(7))
            .unwrap();
        assert_eq!(built_on(&context, "build_image"), Some(7));
        // No output at all reads as the control plane.
        assert_eq!(built_on(&context, "other_job"), None);
    }

    #[test]
    fn paths_that_leave_the_checkout_are_not_identified() {
        assert!(is_contained("."));
        assert!(is_contained("apps/web"));
        assert!(!is_contained("../other"));
        assert!(!is_contained("/etc"));
    }
}
