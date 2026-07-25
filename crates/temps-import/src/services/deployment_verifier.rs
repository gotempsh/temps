//! Deploy-and-verify phase for imports.
//!
//! Creating a project is not a migration. Until the application has actually
//! been built, started, and answered a request, the import has not moved
//! anything — so this module runs the real deployment pipeline and then
//! checks the result, reporting honestly either way.
//!
//! Three outcomes are possible and all three are reported as such:
//!
//! - **Deployed and responding** — the pipeline finished and the app answered
//!   on its temps address.
//! - **Deployed but not verified** — the pipeline finished, but the HTTP check
//!   did not get a healthy response in time. The import is not failed; the
//!   user is told exactly what to look at.
//! - **Not deployed** — the pipeline failed, or could not run at all (for
//!   example an image-based import with no repository linked). The step says
//!   which, and the project is left in place to finish manually.
//!
//! Nothing here ever marks a deployment "completed" that temps did not
//! observe completing.

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_entities::deployments;
use temps_import_types::StepResult;
use tracing::{info, warn};

/// How long to wait for the pipeline to reach a terminal state.
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(600);
/// How long to keep retrying the HTTP check once the deployment is done.
const HTTP_TIMEOUT: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// How long to wait for the creation-time deployment before triggering one.
const TRIGGER_GRACE: Duration = Duration::from_secs(15);

/// Deployment states that mean the pipeline has stopped working.
/// "running" is NOT terminal — in the temps state machine it means the
/// build/deploy workflow is still executing (pending → running → deploying →
/// built → completed | failed | cancelled).
fn is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "error" | "cancelled")
}

fn is_success(state: &str) -> bool {
    state == "completed"
}

/// Result of the deploy-and-verify phase
pub struct VerificationOutcome {
    pub steps: Vec<StepResult>,
    /// The deployment the pipeline actually produced, if any
    pub deployment_id: Option<i32>,
    /// True only when the app was observed answering a request
    pub operational: bool,
    pub warnings: Vec<String>,
}

pub struct DeploymentVerifier {
    db: Arc<DatabaseConnection>,
    deployment_service: Arc<temps_deployments::DeploymentService>,
    http: reqwest::Client,
}

impl DeploymentVerifier {
    pub fn new(
        db: Arc<DatabaseConnection>,
        deployment_service: Arc<temps_deployments::DeploymentService>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            // The imported app's certificate may not be issued yet, and the
            // preview host may still be resolving — a failed TLS handshake
            // must not be reported as "the app is down".
            .danger_accept_invalid_certs(true)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_default();
        Self {
            db,
            deployment_service,
            http,
        }
    }

    /// Deploy the imported project for real, then check that it answers.
    ///
    /// `placeholder_deployment_id` is the importer's own record row — only
    /// deployments newer than it count as the real pipeline run.
    pub async fn deploy_and_verify(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        app_url: Option<String>,
        placeholder_deployment_id: Option<i32>,
    ) -> VerificationOutcome {
        let mut steps = Vec::new();
        let mut warnings = Vec::new();

        let started = Instant::now();
        let before_id = placeholder_deployment_id
            .or(self.latest_deployment_id(project_id, environment_id).await);

        // Project creation already queues an initial deployment when the
        // project has a linked repository. Triggering a second pipeline here
        // would cancel that build mid-flight (the newer deployment supersedes
        // the older), so first give the creation-time deployment a moment to
        // appear and only trigger when nothing started on its own.
        let initial = self
            .wait_for_new_deployment(project_id, environment_id, before_id, TRIGGER_GRACE)
            .await;

        if initial.is_none() {
            if let Err(e) = self
                .deployment_service
                .trigger_pipeline(project_id, environment_id, branch.clone(), None, None)
                .await
            {
                let message = format!(
                    "Could not start a deployment: {} — the project and its services were created, but nothing is running yet. Deploy it from the project page once the cause is resolved (most often: no repository linked, or the image is not reachable).",
                    e
                );
                warnings.push(message.clone());
                steps.push(StepResult {
                    step_id: "deploy-application".to_string(),
                    step_title: "Deploy the imported application".to_string(),
                    success: false,
                    skipped: false,
                    message,
                    created_resources: vec![],
                    duration_seconds: started.elapsed().as_secs_f64(),
                });
                return VerificationOutcome {
                    steps,
                    deployment_id: None,
                    operational: false,
                    warnings,
                };
            }
        }

        // -- Wait for it to finish ----------------------------------------
        let (deployment_id, final_state) = self
            .await_terminal_state(project_id, environment_id, before_id)
            .await;

        let deployed_ok = final_state.as_deref().is_some_and(is_success);
        let deploy_message = match (&final_state, deployed_ok) {
            (Some(state), true) => format!("Deployment finished ({})", state),
            (Some(state), false) => format!(
                "Deployment did not succeed (state: {}) — open the deployment logs on the project page to see which step failed",
                state
            ),
            (None, _) => format!(
                "Deployment did not reach a final state within {}s — it may still be building; check the project page",
                DEPLOY_TIMEOUT.as_secs()
            ),
        };
        if !deployed_ok {
            warnings.push(deploy_message.clone());
        }
        steps.push(StepResult {
            step_id: "deploy-application".to_string(),
            step_title: "Deploy the imported application".to_string(),
            success: deployed_ok,
            skipped: false,
            message: deploy_message,
            created_resources: vec![],
            duration_seconds: started.elapsed().as_secs_f64(),
        });

        if !deployed_ok {
            return VerificationOutcome {
                steps,
                deployment_id,
                operational: false,
                warnings,
            };
        }

        // -- Check the app actually answers -------------------------------
        let verify_started = Instant::now();
        let Some(url) = app_url else {
            steps.push(StepResult {
                step_id: "verify-application".to_string(),
                step_title: "Verify the application responds".to_string(),
                success: true,
                skipped: true,
                message:
                    "No public address is configured for this environment, so temps could not check it automatically — open the project page to confirm the app is serving"
                        .to_string(),
                created_resources: vec![],
                duration_seconds: 0.0,
            });
            return VerificationOutcome {
                steps,
                deployment_id,
                operational: false,
                warnings,
            };
        };

        let probe = self.probe_until_healthy(&url).await;
        let operational = probe.is_some();
        let message = match probe {
            Some(status) => format!("{} responded with HTTP {}", url, status),
            None => {
                let m = format!(
                    "{} did not respond successfully within {}s. The deployment finished, so this is usually DNS or the app failing at startup — check the container logs on the project page.",
                    url,
                    HTTP_TIMEOUT.as_secs()
                );
                warnings.push(m.clone());
                m
            }
        };
        steps.push(StepResult {
            step_id: "verify-application".to_string(),
            step_title: "Verify the application responds".to_string(),
            success: operational,
            skipped: false,
            message,
            created_resources: vec![],
            duration_seconds: verify_started.elapsed().as_secs_f64(),
        });

        if operational {
            info!("Imported project {} is operational at {}", project_id, url);
        }

        VerificationOutcome {
            steps,
            deployment_id,
            operational,
            warnings,
        }
    }

    /// Wait up to `grace` for a deployment newer than `before_id` to appear.
    async fn wait_for_new_deployment(
        &self,
        project_id: i32,
        environment_id: i32,
        before_id: Option<i32>,
        grace: Duration,
    ) -> Option<i32> {
        let deadline = Instant::now() + grace;
        loop {
            let newest = self.latest_deployment_id(project_id, environment_id).await;
            match (newest, before_id) {
                (Some(id), Some(before)) if id > before => return Some(id),
                (Some(id), None) => return Some(id),
                _ => {}
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn latest_deployment_id(&self, project_id: i32, environment_id: i32) -> Option<i32> {
        deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .order_by_desc(deployments::Column::Id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|d| d.id)
    }

    /// Poll until the deployment the pipeline created reaches a terminal
    /// state. Returns the deployment id and its last observed state.
    async fn await_terminal_state(
        &self,
        project_id: i32,
        environment_id: i32,
        before_id: Option<i32>,
    ) -> (Option<i32>, Option<String>) {
        let deadline = Instant::now() + DEPLOY_TIMEOUT;
        let mut last: (Option<i32>, Option<String>) = (None, None);

        while Instant::now() < deadline {
            let newest = deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project_id))
                .filter(deployments::Column::EnvironmentId.eq(environment_id))
                .order_by_desc(deployments::Column::Id)
                .one(self.db.as_ref())
                .await;

            match newest {
                Ok(Some(deployment)) => {
                    // Only consider a deployment the pipeline created after we
                    // triggered it — the importer's own placeholder row must
                    // not be mistaken for a real one.
                    let is_new = before_id.map(|id| deployment.id > id).unwrap_or(true);
                    if is_new {
                        last = (Some(deployment.id), Some(deployment.state.clone()));
                        if is_terminal(&deployment.state) {
                            return last;
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => warn!("Could not read deployment state during import: {}", e),
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        last
    }

    /// Request the app until it answers with a non-server-error status.
    /// Returns the status code that satisfied the check.
    async fn probe_until_healthy(&self, url: &str) -> Option<u16> {
        let deadline = Instant::now() + HTTP_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(response) = self.http.get(url).send().await {
                let status = response.status();
                // Any non-5xx answer proves the app is serving. A 401/404 is
                // still the imported app responding — it is not our place to
                // decide the route should have existed.
                if !status.is_server_error() {
                    return Some(status.as_u16());
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_cover_success_and_failure() {
        for state in ["completed", "failed", "error", "cancelled"] {
            assert!(is_terminal(state), "{state} should be terminal");
        }
        // "running" means the workflow is still executing — waiting must
        // continue or the check would pass before the app even built.
        for state in ["pending", "running", "deploying", "built"] {
            assert!(!is_terminal(state), "{state} should not be terminal");
        }
    }

    #[test]
    fn only_finished_states_count_as_success() {
        assert!(is_success("completed"));
        for state in [
            "failed",
            "error",
            "cancelled",
            "running",
            "deploying",
            "pending",
        ] {
            assert!(!is_success(state), "{state} must not count as success");
        }
    }
}
