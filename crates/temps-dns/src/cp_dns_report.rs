// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's own resolver reports itself (ADR-095 §3).
//!
//! The control-plane resolver runs in the proxy process, which in a split
//! deployment is not the console's. So the proxy's DNS bootstrap loop owns
//! a [`ControlPlaneDnsReporter`] and, every [`CP_REPORT_INTERVAL`], stores
//! its resolver's report as node 0 through the same
//! [`DnsObservabilityService::ingest`] the console uses for workers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use temps_node_dns::observe::{DnsObserver, DnsReport, DnsResolverState};
use tracing::{debug, warn};

use crate::cp_resolver::ControlPlaneResolver;
use crate::services::observability::{
    DnsObservabilityService, ReportOutcome, ReportState, CONTROL_PLANE_NODE_ID,
};

/// How often node 0 reports. The console pulls workers on the monitoring
/// scrape interval, 30 s by default.
pub const CP_REPORT_INTERVAL: Duration = Duration::from_secs(30);

/// Version of the binary running the control plane's resolver.
const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Node 0's report: the observer's counters and log, the resolver's state,
/// or why it is not running.
pub fn control_plane_report(
    observer: &DnsObserver,
    resolver: Option<&ControlPlaneResolver>,
    bootstrap_error: Option<&str>,
) -> DnsReport {
    let state = match resolver.and_then(|r| r.view().map(|view| (r, view))) {
        Some((resolver, view)) => view.state(resolver.tasks_alive()),
        None => DnsResolverState {
            last_error: bootstrap_error.map(str::to_string),
            ..DnsResolverState::default()
        },
    };
    // The ring is small; ingest skips what it already stored.
    DnsReport::assemble(observer.snapshot(None, None), BUILD_VERSION, state)
}

/// What node 0 stores: its resolver's report, or, when it cannot run one
/// where it runs (`unsupported` says why), that instead of an outage.
pub fn control_plane_outcome(
    observer: &DnsObserver,
    resolver: Option<&ControlPlaneResolver>,
    bootstrap_error: Option<&str>,
    unsupported: Option<&str>,
) -> ReportOutcome {
    match unsupported {
        Some(reason) => ReportOutcome::Failed {
            state: ReportState::Unsupported,
            reason: reason.to_string(),
        },
        None => ReportOutcome::Report(Box::new(control_plane_report(
            observer,
            resolver,
            bootstrap_error,
        ))),
    }
}

/// When node 0 is due to report: at most once per [`CP_REPORT_INTERVAL`],
/// and at once after [`ReportGate::expire`].
#[derive(Debug, Default)]
struct ReportGate {
    last: Option<Instant>,
}

impl ReportGate {
    /// Whether a report is due at `now`; if so, it counts as made.
    fn take(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < CP_REPORT_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        true
    }

    fn expire(&mut self) {
        self.last = None;
    }
}

/// Reports node 0 at most once per [`CP_REPORT_INTERVAL`].
pub struct ControlPlaneDnsReporter {
    observer: Arc<DnsObserver>,
    service: Arc<DnsObservabilityService>,
    gate: ReportGate,
}

impl ControlPlaneDnsReporter {
    /// A reporter with a new observer. Must be called within a Tokio
    /// runtime.
    pub fn new(service: Arc<DnsObservabilityService>) -> Self {
        Self {
            observer: DnsObserver::spawn(),
            service,
            gate: ReportGate::default(),
        }
    }

    /// Make the next [`maybe_report`](Self::maybe_report) store a report
    /// without waiting out the interval: called when what node 0 would say
    /// changed (cluster DNS turned on or off, the host found able or unable
    /// to run a resolver), so the stored report never describes the old state.
    pub fn expire(&mut self) {
        self.gate.expire();
    }

    /// The observer every control-plane resolver start must be given.
    pub fn observer(&self) -> Arc<DnsObserver> {
        self.observer.clone()
    }

    /// Store a report if the interval has passed since the last one.
    ///
    /// `internal_proxy` is how the control plane serves `*.temps.local` HTTP
    /// calls to its own containers (ADR-104 D4), in the heartbeat document's
    /// shape; `None` while it does not run the proxy. `unsupported` is set
    /// when no resolver can run on this host, and says why.
    pub async fn maybe_report(
        &mut self,
        resolver: Option<&ControlPlaneResolver>,
        bootstrap_error: Option<&str>,
        unsupported: Option<&str>,
        internal_proxy: Option<serde_json::Value>,
    ) {
        if !self.gate.take(Instant::now()) {
            return;
        }
        let outcome = control_plane_outcome(&self.observer, resolver, bootstrap_error, unsupported);
        match self
            .service
            .ingest(CONTROL_PLANE_NODE_ID, outcome, chrono::Utc::now())
            .await
        {
            Ok(summary) => debug!(
                queries_logged = summary.queries_logged,
                points = summary.points_written,
                "Stored the control-plane DNS resolver report"
            ),
            Err(error) => {
                warn!(
                    error = %error,
                    "Could not store the control-plane DNS resolver report; the next interval retries"
                );
                return;
            }
        }
        if let Err(error) = self
            .service
            .record_control_plane_internal_proxy(internal_proxy)
            .await
        {
            warn!(
                error = %error,
                "Could not store the control plane's internal routing state; the next interval retries"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_once_per_interval_unless_expired() {
        let start = Instant::now();
        let mut gate = ReportGate::default();
        assert!(gate.take(start), "the first report is due at once");
        assert!(!gate.take(start + Duration::from_secs(5)));
        assert!(gate.take(start + CP_REPORT_INTERVAL));

        // A change of state is reported on the next pass, not an interval later.
        let changed = start + CP_REPORT_INTERVAL + Duration::from_secs(5);
        assert!(!gate.take(changed));
        gate.expire();
        assert!(gate.take(changed));
        assert!(!gate.take(changed + Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn a_stopped_resolver_reports_why() {
        let observer = DnsObserver::spawn();
        let report = control_plane_report(&observer, None, Some("port 53 is in use"));
        assert!(!report.resolver.running);
        assert_eq!(
            report.resolver.last_error.as_deref(),
            Some("port 53 is in use")
        );
        assert_eq!(report.instance_id, observer.instance_id());
        assert_eq!(report.build_version, BUILD_VERSION);

        let off = control_plane_report(&observer, None, None);
        assert!(!off.resolver.running);
        assert_eq!(off.resolver.last_error, None);
    }

    #[tokio::test]
    async fn a_host_that_cannot_run_a_resolver_reports_unsupported_not_down() {
        let observer = DnsObserver::spawn();
        let why = "172.20.0.1 is not an address of this machine";
        match control_plane_outcome(&observer, None, Some("bind failed"), Some(why)) {
            ReportOutcome::Failed { state, reason } => {
                assert_eq!(state, ReportState::Unsupported);
                assert_eq!(reason, why);
            }
            ReportOutcome::Report(_) => panic!("expected an unsupported outcome"),
        }
        match control_plane_outcome(&observer, None, Some("bind failed"), None) {
            ReportOutcome::Report(report) => {
                assert_eq!(report.resolver.last_error.as_deref(), Some("bind failed"));
            }
            ReportOutcome::Failed { .. } => panic!("a failure on a capable host is a report"),
        }
        assert_eq!(
            ReportState::parse("unsupported"),
            Some(ReportState::Unsupported)
        );
    }
}
