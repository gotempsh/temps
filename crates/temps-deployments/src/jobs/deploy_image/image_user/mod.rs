// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which user a deployment's image runs as, checked once per deployment.
//!
//! Project containers run as the image's own `USER`, and an image that
//! declares none runs as root (uid 0). Before any replica starts, the deploy
//! job resolves that user once, on the node that holds the image, against
//! the image's own `/etc/passwd` and `/etc/group`, with the same resolver a
//! microVM replica is started with ([`temps_docker_util::image_user`]). It
//! then writes one warning line to the deployment log when the answer is
//! root, and records the answer on the deployment row, where the API and
//! console read it. Nothing inspects an image on an API request.
//!
//! The check is advisory: it never fails or delays a deploy beyond its own
//! deadline, which covers the whole step (reaching the node, the check, the
//! log line and the row update). Anything it cannot determine (an older
//! agent, an unreadable image, a named user the image's files do not list, a
//! `USER` too long to be reported) is logged as such and never reported as
//! root. A multi-architecture build is checked per architecture's image, and
//! warned about when any of them runs as root.

use std::time::Duration;

use sea_orm::{sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter};
use temps_deployer::ImageBuilder;
use temps_deployment_logs::LogLevel;
use temps_docker_util::image_user::{
    classify_image_user, collect_identity_archive, read_identity_files, FetchedPath,
    IdentityFilesNeeded, ImageFileError, ImageUserVerdict,
};

use super::{BuildImageOutput, DeployImageJob};
use crate::jobs::build_image::DOCKERFILE_PRESET_OUTPUT;
use crate::workflow::WorkflowContext;

/// Where the console and the deploy log send someone who wants the fix.
pub(crate) const NON_ROOT_USER_DOCS_URL: &str =
    "https://temps.sh/docs/docker-containers#non-root-user";

/// The whole advisory step, so an unresponsive node or a locked deployment
/// row cannot hold up the deploy it only advises on.
const IMAGE_USER_STEP_TIMEOUT: Duration = Duration::from_secs(60);

/// One image's check (inspect plus at most two small file reads). Every
/// image is checked at once, within three quarters of the step; what is
/// left of it writes the outcome.
const IMAGE_USER_CHECK_TIMEOUT: Duration = Duration::from_secs(45);

/// Longest `USER` recorded on the deployment row. The daemon already drops a
/// `USER` past `MAX_IMAGE_CONFIG_USER_BYTES` from the image configuration;
/// this keeps the column bounded whatever a future source reports.
const MAX_RECORDED_IMAGE_USER_CHARS: usize = 256;

/// What built the deployed image, which decides what the root warning tells
/// the user to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImageOrigin {
    /// Temps generated the Dockerfile from this preset.
    Preset(String),
    /// The repository's own Dockerfile.
    RepositoryDockerfile,
    /// An image Temps did not build in this deployment: a registry image, an
    /// upload, a rollback or a promotion.
    PrebuiltImage,
}

/// The outcome of checking an image's user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImageUserCheck {
    /// The image was inspected. `declared` is its `USER` as written, `None`
    /// when it declares none.
    Checked {
        declared: Option<String>,
        verdict: ImageUserVerdict,
    },
    /// The image could not be inspected or its identity files read.
    NotChecked { reason: String },
}

/// Resolve the user `image` runs as, through `builder`, the image store that
/// holds it (the control plane's daemon or a worker's agent).
pub(super) async fn check_image_user(builder: &dyn ImageBuilder, image: &str) -> ImageUserCheck {
    match tokio::time::timeout(
        IMAGE_USER_CHECK_TIMEOUT,
        inspect_and_classify(builder, image),
    )
    .await
    {
        Ok(check) => check,
        Err(_) => ImageUserCheck::NotChecked {
            reason: format!(
                "checking which user image '{image}' runs as took longer than {}s",
                IMAGE_USER_CHECK_TIMEOUT.as_secs()
            ),
        },
    }
}

async fn inspect_and_classify(builder: &dyn ImageBuilder, image: &str) -> ImageUserCheck {
    let info = match builder.inspect_image(image).await {
        Ok(info) => info,
        Err(error) => {
            return ImageUserCheck::NotChecked {
                reason: format!("inspecting image '{image}': {error}"),
            }
        }
    };
    let Some(config) = info.config else {
        return ImageUserCheck::NotChecked {
            reason: format!(
                "the image store holding '{image}' did not report its configuration (an agent \
                 that predates it)"
            ),
        };
    };
    if config.user_omitted {
        return ImageUserCheck::NotChecked {
            reason: format!(
                "image '{image}' sets a USER longer than {} bytes, which is not read",
                temps_deployer::MAX_IMAGE_CONFIG_USER_BYTES
            ),
        };
    }
    // Read the files of the image just inspected, by its id, so a tag moved
    // in the meantime cannot pair this `USER` with another image's passwd.
    let source = if info.id.starts_with("sha256:") {
        info.id.as_str()
    } else {
        image
    };
    let declared = config.user;
    let spec = declared.as_deref().unwrap_or("");
    let identity =
        match read_identity_files(image, IdentityFilesNeeded::to_classify(spec), |path| {
            fetch_image_path(builder, source, image, path)
        })
        .await
        {
            Ok(identity) => identity,
            Err(error) => {
                return ImageUserCheck::NotChecked {
                    reason: error.to_string(),
                }
            }
        };
    ImageUserCheck::Checked {
        verdict: classify_image_user(spec, &identity),
        declared,
    }
}

/// One path of `image` as a container-archive tar, through the builder's
/// path export (served by a worker's agent for an image it built). `source`
/// is what is exported (the inspected image id); `image` names it in errors.
///
/// A path the image lacks is an export error here, not
/// [`FetchedPath::Missing`]: the export cannot tell the two apart, so the
/// check reports it as not checked rather than guessing.
async fn fetch_image_path(
    builder: &dyn ImageBuilder,
    source: &str,
    image: &str,
    path: String,
) -> Result<FetchedPath, ImageFileError> {
    let stream = builder
        .export_path_stream(source, &path)
        .await
        .map_err(|error| ImageFileError::Docker {
            image: image.to_string(),
            path: path.clone(),
            reason: error.to_string(),
        })?;
    collect_identity_archive(stream, image, &path)
        .await
        .map(FetchedPath::Archive)
}

/// The suggested change, shared by every warning.
const NON_ROOT_USER_EXAMPLE: &str = "e.g. `RUN useradd -r -u 10001 app` and `USER app` \
     (on Alpine: `RUN adduser -S -u 10001 app`)";

/// The deployment-log warning for an image that runs as root. `variant`
/// names the image when the build produced one per architecture.
pub(super) fn root_user_warning(origin: &ImageOrigin, variant: Option<&str>) -> String {
    let advice = match origin {
        ImageOrigin::Preset(preset) => format!(
            "The Temps '{preset}' preset that built it does not set a non-root USER. To run \
             as a non-root user, add your own Dockerfile to the repository with a non-root \
             USER, {NON_ROOT_USER_EXAMPLE}"
        ),
        ImageOrigin::RepositoryDockerfile => {
            format!("Add a non-root USER to your Dockerfile, {NON_ROOT_USER_EXAMPLE}")
        }
        ImageOrigin::PrebuiltImage => format!(
            "Rebuild the image with a non-root USER in its Dockerfile, {NON_ROOT_USER_EXAMPLE}"
        ),
    };
    let subject = match variant {
        Some(image) => format!("This deployment's image {image}"),
        None => "This image".to_string(),
    };
    format!(
        "⚠️ {subject} runs as root (uid 0). {advice}. The deployment continues; see \
         {NON_ROOT_USER_DOCS_URL}"
    )
}

/// The deployment-log line for every other outcome.
fn image_user_note(check: &ImageUserCheck) -> Option<String> {
    match check {
        ImageUserCheck::Checked {
            verdict: ImageUserVerdict::Root,
            ..
        } => None,
        ImageUserCheck::Checked {
            declared,
            verdict: ImageUserVerdict::NonRoot { uid },
        } => Some(format!(
            "Image runs as user '{}' (uid {uid})",
            declared.as_deref().unwrap_or_default()
        )),
        ImageUserCheck::Checked {
            verdict: ImageUserVerdict::Unresolved(error),
            ..
        } => Some(format!(
            "Could not tell which uid the image runs as, so it is not reported as root: {error}"
        )),
        ImageUserCheck::NotChecked { reason } => Some(format!(
            "Could not check which user the image runs as ({reason}); it is not reported as root"
        )),
    }
}

/// The deployment's verdict from one check per image: the first image that
/// runs as root (so a root architecture is never hidden behind a non-root
/// one), otherwise the primary image's check. `checks` starts with the
/// primary image.
fn combine_checks(checks: Vec<(String, ImageUserCheck)>) -> Option<(String, ImageUserCheck)> {
    let root = checks.iter().position(|(_, check)| {
        matches!(
            check,
            ImageUserCheck::Checked {
                verdict: ImageUserVerdict::Root,
                ..
            }
        )
    });
    let mut checks = checks;
    match root {
        Some(index) => Some(checks.swap_remove(index)),
        None => checks.into_iter().next(),
    }
}

/// `USER` as stored on the deployment row: bounded, `None` when absent.
fn recorded_image_user(declared: Option<&str>) -> Option<String> {
    declared.map(|user| user.chars().take(MAX_RECORDED_IMAGE_USER_CHARS).collect())
}

impl DeployImageJob {
    /// What built this deployment's image.
    pub(super) fn image_origin(&self, context: &WorkflowContext) -> ImageOrigin {
        if self.external_image_tag.is_some() {
            return ImageOrigin::PrebuiltImage;
        }
        match context.get_output::<String>(&self.build_job_id, DOCKERFILE_PRESET_OUTPUT) {
            Ok(Some(preset)) => ImageOrigin::Preset(preset),
            _ => ImageOrigin::RepositoryDockerfile,
        }
    }

    /// Check which user the deployment's image runs as, log it, and record it
    /// on the deployment row. Never fails the deployment, and never holds it
    /// up past [`IMAGE_USER_STEP_TIMEOUT`].
    pub(super) async fn check_and_record_image_user(
        &self,
        image_output: &BuildImageOutput,
        context: &WorkflowContext,
    ) {
        self.check_and_record_image_user_within(image_output, context, IMAGE_USER_STEP_TIMEOUT)
            .await;
    }

    pub(super) async fn check_and_record_image_user_within(
        &self,
        image_output: &BuildImageOutput,
        context: &WorkflowContext,
        deadline: Duration,
    ) {
        // The checks get most of the step; the rest writes their outcome.
        let checks_deadline = deadline.mul_f64(0.75);
        if tokio::time::timeout(
            deadline,
            self.check_log_and_record(image_output, context, checks_deadline),
        )
        .await
        .is_err()
        {
            // Nothing to clean up here: a path export removes its scratch
            // container through a guard that outlives this future.
            tracing::warn!(
                project_id = context.project_id,
                deployment_id = context.deployment_id,
                image = image_output.image_tag.as_str(),
                deadline_secs = deadline.as_secs(),
                "The image-user check did not finish in time; the deployment continues without it"
            );
        }
    }

    /// Check one image, through the image store that holds it.
    async fn check_one_image(
        &self,
        image_output: &BuildImageOutput,
        image: &str,
    ) -> ImageUserCheck {
        match image_output.builder_node_for_tag(image) {
            Some(node_id) => match self.remote_deployer_for_node_id(node_id).await {
                Ok(node) => check_image_user(node.as_ref(), image).await,
                Err(error) => ImageUserCheck::NotChecked {
                    reason: format!("reaching build node {node_id}: {error}"),
                },
            },
            None => match self.image_builder.as_ref() {
                Some(local) => check_image_user(local.as_ref(), image).await,
                None => ImageUserCheck::NotChecked {
                    reason: "this control plane has no image store to inspect the image in"
                        .to_string(),
                },
            },
        }
    }

    async fn check_log_and_record(
        &self,
        image_output: &BuildImageOutput,
        context: &WorkflowContext,
        checks_deadline: Duration,
    ) {
        // One image per architecture the build produced, the primary first.
        let mut images = vec![image_output.image_tag.clone()];
        let mut variants: Vec<&String> = image_output.image_tags_by_platform.values().collect();
        variants.sort();
        for tag in variants {
            if !images.contains(tag) {
                images.push(tag.clone());
            }
        }
        // Each check's result is kept as it lands, so one image still
        // pending at the deadline cannot discard another's verdict (a
        // confirmed root warning above all); only the unfinished ones are
        // reported as not checked.
        let deadline = tokio::time::Instant::now() + checks_deadline;
        let mut results: Vec<Option<ImageUserCheck>> = vec![None; images.len()];
        let mut pending: futures::stream::FuturesUnordered<_> = images
            .iter()
            .enumerate()
            .map(|(index, image)| async move {
                (index, self.check_one_image(image_output, image).await)
            })
            .collect();
        while let Ok(Some((index, check))) =
            tokio::time::timeout_at(deadline, futures::StreamExt::next(&mut pending)).await
        {
            results[index] = Some(check);
        }
        drop(pending);
        let checks: Vec<(String, ImageUserCheck)> = images
            .iter()
            .cloned()
            .zip(results)
            .map(|(image, check)| {
                let check = check.unwrap_or_else(|| ImageUserCheck::NotChecked {
                    reason: format!(
                        "checking which user image '{image}' runs as took longer than {}s",
                        checks_deadline.as_secs()
                    ),
                });
                (image, check)
            })
            .collect();
        let several = images.len() > 1;
        let Some((image, check)) = combine_checks(checks) else {
            return;
        };
        let image = image.as_str();

        let (level, message) = match image_user_note(&check) {
            None => (
                LogLevel::Warning,
                root_user_warning(&self.image_origin(context), several.then_some(image)),
            ),
            Some(note) => (LogLevel::Info, note),
        };
        tracing::info!(
            project_id = context.project_id,
            deployment_id = context.deployment_id,
            image,
            check = ?check,
            "Checked which user the deployment's image runs as"
        );
        if let Err(error) = self.log_at(context, level, message).await {
            tracing::warn!(
                deployment_id = context.deployment_id,
                error = %error,
                "Failed to write the image-user line to the deployment log"
            );
        }
        self.persist_image_user(context, &check).await;
    }

    /// Record a checked image's user on the deployment row. Best effort, like
    /// [`Self::persist_docker_socket_mounted`]: the replicas have not started
    /// yet and must not be refused over an advisory field.
    pub(super) async fn persist_image_user(
        &self,
        context: &WorkflowContext,
        check: &ImageUserCheck,
    ) {
        let ImageUserCheck::Checked { declared, verdict } = check else {
            return;
        };
        let (Some(db), Some(deployment_id)) =
            (self.failed_container_db.as_ref(), self.deployment_id)
        else {
            tracing::warn!(
                project_id = context.project_id,
                deployment_id = context.deployment_id,
                "No database handle wired into this job; the image user was logged but not \
                 recorded on the deployment row"
            );
            return;
        };
        if let Err(error) = temps_entities::deployments::Entity::update_many()
            .col_expr(
                temps_entities::deployments::Column::RunsAsRoot,
                Expr::value(verdict.is_root()),
            )
            .col_expr(
                temps_entities::deployments::Column::ImageUser,
                Expr::value(recorded_image_user(declared.as_deref())),
            )
            .filter(temps_entities::deployments::Column::Id.eq(deployment_id))
            .exec(db.as_ref())
            .await
        {
            tracing::error!(
                project_id = context.project_id,
                deployment_id,
                error = %error,
                "Failed to record the image user on the deployment row"
            );
        }
    }
}

#[cfg(test)]
mod tests;
