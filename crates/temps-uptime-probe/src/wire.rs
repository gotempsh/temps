// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the control plane sends a worker node's agent to run one check from
//! there (ADR 108 D5), and what comes back.
//!
//! The request carries the check's decrypted headers and body, so the
//! control plane only sends it sealed to the node. The agent validates it
//! exactly as the control plane does on save and probes through the same
//! public-address policy, so a worker cannot be made to reach an address the
//! control plane would refuse.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::CheckConfigError;
use crate::probe::{ProbeOutcome, TargetProber};
use crate::spec::{parse_target, validate_spec, CheckRequest, CheckSpec};

/// The agent route that runs one check.
pub const REMOTE_PROBE_PATH: &str = "/agent/uptime/probe";

/// One check to run from a worker node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemoteProbeRequest {
    pub check: CheckSpec,
    /// The target as stored (`MonitorCheckView.target`).
    pub target: String,
    #[serde(default)]
    pub request: CheckRequest,
}

impl RemoteProbeRequest {
    /// Validate the request as the control plane validates it on save, then
    /// probe. An invalid request is refused rather than probed.
    ///
    /// The whole probe, name resolution included, is bounded by the check's
    /// timeout: an HTTP client's timeout starts after DNS, so a slow resolver
    /// would otherwise hold one of the agent's few probe slots long after the
    /// control plane stopped waiting. A probe past its timeout reads as down.
    pub async fn run(self, prober: &dyn TargetProber) -> Result<ProbeOutcome, CheckConfigError> {
        let check = validate_spec(self.check, &self.request)?;
        let target = parse_target(check.kind(), &self.target)?;
        let timeout = check.timeout();
        match tokio::time::timeout(timeout, prober.probe(&check, &target, &self.request)).await {
            Ok(outcome) => Ok(outcome),
            Err(_) => Ok(ProbeOutcome::down(
                Some(timeout),
                format!(
                    "No answer from {} within {} ms, name resolution included",
                    target.stored(),
                    timeout.as_millis()
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::probe::ProbeStatus;
    use crate::spec::{Target, TcpCheck};

    struct Recording;

    #[async_trait]
    impl TargetProber for Recording {
        async fn probe(&self, _: &CheckSpec, target: &Target, _: &CheckRequest) -> ProbeOutcome {
            ProbeOutcome {
                status: ProbeStatus::Operational,
                response_time_ms: Some(1),
                message: Some(target.stored()),
            }
        }
    }

    fn tcp(target: &str) -> RemoteProbeRequest {
        RemoteProbeRequest {
            check: CheckSpec::Tcp(TcpCheck {
                degraded_after_ms: None,
                timeout_ms: None,
            }),
            target: target.to_string(),
            request: CheckRequest::default(),
        }
    }

    #[tokio::test]
    async fn validates_before_probing() {
        let outcome = tcp("db.example.com:5432").run(&Recording).await.unwrap();
        assert_eq!(outcome.message.as_deref(), Some("db.example.com:5432"));
        let error = tcp("db.example.com").run(&Recording).await.unwrap_err();
        assert!(error.to_string().contains("port"), "{error}");
    }

    struct Hanging;

    #[async_trait]
    impl TargetProber for Hanging {
        async fn probe(&self, _: &CheckSpec, _: &Target, _: &CheckRequest) -> ProbeOutcome {
            // A resolver that never answers.
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_probe_that_never_answers_is_down_after_the_check_timeout() {
        let mut request = tcp("db.example.com:5432");
        request.check = CheckSpec::Tcp(TcpCheck {
            degraded_after_ms: None,
            timeout_ms: Some(2_000),
        });
        let outcome = request.run(&Hanging).await.unwrap();
        assert_eq!(outcome.status, ProbeStatus::Down);
        assert_eq!(outcome.response_time_ms, Some(2_000));
        let message = outcome.message.unwrap();
        assert!(
            message.contains("db.example.com:5432") && message.contains("2000 ms"),
            "{message}"
        );
    }

    #[test]
    fn outcome_status_serializes_as_the_stored_value() {
        let down = ProbeOutcome {
            status: ProbeStatus::Down,
            response_time_ms: None,
            message: Some("refused".into()),
        };
        let json = serde_json::to_value(&down).unwrap();
        assert_eq!(json["status"], "major_outage");
        assert_eq!(serde_json::from_value::<ProbeOutcome>(json).unwrap(), down);
    }
}
