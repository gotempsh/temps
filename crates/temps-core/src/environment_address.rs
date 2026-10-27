// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One primary address per environment, and the warning a deploy carries
//! when nobody can open it (ADR 068 D4, D5).
//!
//! The ingress plugin knows which of an environment's addresses are
//! reachable (ADR 064 D3) and implements [`EnvironmentAddresses`]; the
//! deployments, projects and status page crates only read it, through an
//! [`EnvironmentAddressSlot`] the ingress plugin registers early and fills
//! once every plugin has registered. A binary without the ingress plugin has
//! no slot, and every reader keeps the behaviour it had before ADR 068.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

/// What kind of address the primary one is, in D4's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressKind {
    /// A custom domain, or a hostname the user bound to the environment.
    CustomDomain,
    /// The environment domain under a tunnel's zone (ADR 068 D3).
    TunnelHost,
    /// The generated hostname under the preview domain.
    Generated,
    /// An address only people on a private network can open.
    Private,
}

/// The address every surface means by "the app's address".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PrimaryAddress {
    /// The hostname, without a scheme.
    #[schema(example = "myapp.example.com")]
    pub address: String,
    pub kind: AddressKind,
    /// How traffic gets in, as ADR 064 D3 names it (`tunnel:<name>`,
    /// `direct:<node>`, `private:<node>`).
    #[schema(example = "tunnel:homelab")]
    pub way_in: Option<String>,
}

/// D2: whether the preview domain can be reached at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewDomainStatus {
    /// `localhost`, `localho.st` or a name under them.
    LocalOnly,
    /// A probe name under it does not resolve.
    Unresolved,
    /// It resolves, but to nothing this install serves publicly.
    NoWayIn,
    /// It resolves to a public way in of this install.
    Usable,
}

impl PreviewDomainStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewDomainStatus::LocalOnly => "local_only",
            PreviewDomainStatus::Unresolved => "unresolved",
            PreviewDomainStatus::NoWayIn => "no_way_in",
            PreviewDomainStatus::Usable => "usable",
        }
    }
}

/// One way to fix a [`ReachabilityWarning`], in the order D5 lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReachabilityFixKind {
    /// Give every environment a hostname under a tunnel's zone (D3).
    TunnelEnvironmentHosts,
    /// Set a wildcard preview domain.
    PreviewDomain,
    /// Add a domain to this environment.
    CustomDomain,
    /// Let a worker take public traffic.
    WorkerIngress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReachabilityFix {
    pub kind: ReachabilityFixKind,
    /// What the button and the log line say.
    #[schema(example = "Publish it through tunnel 'homelab' as myapp-production.example.com")]
    pub label: String,
    /// The console page that applies it.
    pub href: Option<String>,
    /// The command to run, when that is the fix.
    pub command: Option<String>,
    /// For the tunnel fix: the connector that would carry it.
    pub connector_id: Option<i32>,
    /// For the tunnel fix: the hostname it would get.
    pub hostname: Option<String>,
}

/// D5: a successful deploy nobody can open. Computed, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReachabilityWarning {
    pub environment_id: i32,
    /// The environment's label, as visitors would see it in a hostname.
    #[schema(example = "myapp-production")]
    pub environment: String,
    /// `Deployed, but nobody can reach <label> yet`.
    pub headline: String,
    /// Why, about the address visitors would try first.
    pub reason: String,
    /// The fixes that fit this install, best first.
    pub fixes: Vec<ReachabilityFix>,
}

impl ReachabilityWarning {
    /// The deploy log's final block (D5.1).
    pub fn log_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("⚠ {}:", self.headline),
            format!("  {}", self.reason),
        ];
        if !self.fixes.is_empty() {
            lines.push("  Make it reachable:".to_string());
            for (index, fix) in self.fixes.iter().enumerate() {
                let label = match &fix.command {
                    Some(command) => format!("{}: {command}", fix.label),
                    None => fix.label.clone(),
                };
                let label = lower_first(&label);
                lines.push(if index == 0 {
                    format!("    • {label}")
                } else {
                    format!("    • or {label}")
                });
            }
        }
        lines
    }

    /// The same words as one paragraph, for a notification.
    pub fn message(&self) -> String {
        self.log_lines()[1..]
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The status a monitor's check records when its environment has no address
/// anyone can reach (D4): nothing was probed, so it is neither up nor down.
/// Every reader of `status_checks.status` must skip it.
pub const NO_ADDRESS_CHECK_STATUS: &str = "no_address";

/// What an environment's address is right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentAddress {
    pub environment_id: i32,
    /// `None` when no address is reachable, or Temps cannot tell.
    pub primary: Option<PrimaryAddress>,
    /// Set only when reachability reports that no address is reachable:
    /// nobody can open the environment. Never set while Temps is still
    /// checking or cannot tell (a [`PrimaryAddress`] is then absent too).
    pub no_address_reason: Option<String>,
    /// D5, for a deployed hosted environment.
    pub warning: Option<ReachabilityWarning>,
}

impl EnvironmentAddress {
    /// The address is settled as unreachable (D4: the open button is
    /// disabled, the monitor does not probe).
    pub fn has_no_address(&self) -> bool {
        self.primary.is_none() && self.no_address_reason.is_some()
    }
}

#[derive(Debug, Error)]
#[error(
    "Could not tell the address of environment {environment_id} in project {project_id}: {reason}"
)]
pub struct EnvironmentAddressError {
    pub project_id: i32,
    pub environment_id: i32,
    pub reason: String,
}

#[async_trait]
pub trait EnvironmentAddresses: Send + Sync {
    /// The environment's address, at most a minute old.
    async fn address(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<EnvironmentAddress, EnvironmentAddressError>;

    /// The last address computed for it, without computing one: for lists,
    /// which must not resolve DNS per row.
    fn cached(&self, environment_id: i32) -> Option<EnvironmentAddress>;

    /// A deployment of the environment just went live: recompute its
    /// address, open or close its D5 notification episode, and return the
    /// warning for the deploy log. Never fails the deploy.
    async fn deployment_completed(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Option<ReachabilityWarning>;
}

/// Where the ingress plugin puts its [`EnvironmentAddresses`] once every
/// plugin has registered. Empty until then, and forever in a binary without
/// the ingress plugin.
#[derive(Default)]
pub struct EnvironmentAddressSlot {
    inner: OnceLock<Arc<dyn EnvironmentAddresses>>,
}

impl EnvironmentAddressSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fill the slot. A second call is ignored: the first implementation
    /// stays.
    pub fn set(&self, addresses: Arc<dyn EnvironmentAddresses>) -> bool {
        self.inner.set(addresses).is_ok()
    }

    pub fn get(&self) -> Option<Arc<dyn EnvironmentAddresses>> {
        self.inner.get().cloned()
    }
}

/// A URL for `address` on `https`, the scheme every public hostname of a
/// custom domain or tunnel host is served on.
pub fn https_url(address: &str) -> String {
    if address.contains("://") {
        address.to_string()
    } else {
        format!("https://{address}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warning(fixes: Vec<ReachabilityFix>) -> ReachabilityWarning {
        ReachabilityWarning {
            environment_id: 7,
            environment: "hello-temps-production".into(),
            headline: "Deployed, but nobody can reach hello-temps-production yet".into(),
            reason: "hello-temps-production.localho.st only resolves to this computer.".into(),
            fixes,
        }
    }

    fn fix(kind: ReachabilityFixKind, label: &str) -> ReachabilityFix {
        ReachabilityFix {
            kind,
            label: label.into(),
            href: None,
            command: None,
            connector_id: None,
            hostname: None,
        }
    }

    #[test]
    fn the_log_block_lists_the_fixes_in_order() {
        let block = warning(vec![
            fix(
                ReachabilityFixKind::TunnelEnvironmentHosts,
                "Publish it through tunnel 'edge' as hello-temps-production.example.com",
            ),
            fix(
                ReachabilityFixKind::PreviewDomain,
                "Set a wildcard preview domain in Settings → Platform",
            ),
            fix(
                ReachabilityFixKind::CustomDomain,
                "Add a domain to this environment",
            ),
        ])
        .log_lines();
        assert_eq!(
            block,
            vec![
                "⚠ Deployed, but nobody can reach hello-temps-production yet:",
                "  hello-temps-production.localho.st only resolves to this computer.",
                "  Make it reachable:",
                "    • publish it through tunnel 'edge' as hello-temps-production.example.com",
                "    • or set a wildcard preview domain in Settings → Platform",
                "    • or add a domain to this environment",
            ]
        );
    }

    #[test]
    fn a_command_fix_prints_the_command() {
        let mut worker = fix(
            ReachabilityFixKind::WorkerIngress,
            "Let a worker take public traffic",
        );
        worker.command = Some("temps agent ingress delegated".into());
        let block = warning(vec![worker]).log_lines();
        assert_eq!(
            block.last().map(String::as_str),
            Some("    • let a worker take public traffic: temps agent ingress delegated")
        );
        assert!(warning(vec![])
            .message()
            .starts_with("hello-temps-production.localho.st"));
    }

    #[test]
    fn no_address_is_only_set_when_settled() {
        let mut address = EnvironmentAddress {
            environment_id: 1,
            primary: None,
            no_address_reason: None,
            warning: None,
        };
        // Checking or unknown: not "no address".
        assert!(!address.has_no_address());
        address.no_address_reason = Some("nobody can reach it".into());
        assert!(address.has_no_address());
        address.primary = Some(PrimaryAddress {
            address: "app.example.com".into(),
            kind: AddressKind::CustomDomain,
            way_in: None,
        });
        assert!(!address.has_no_address());
    }

    #[test]
    fn the_slot_keeps_its_first_value() {
        struct Fixed(i32);
        #[async_trait]
        impl EnvironmentAddresses for Fixed {
            async fn address(
                &self,
                _project_id: i32,
                environment_id: i32,
            ) -> Result<EnvironmentAddress, EnvironmentAddressError> {
                Ok(EnvironmentAddress {
                    environment_id: environment_id + self.0,
                    primary: None,
                    no_address_reason: None,
                    warning: None,
                })
            }
            fn cached(&self, _environment_id: i32) -> Option<EnvironmentAddress> {
                None
            }
            async fn deployment_completed(&self, _: i32, _: i32) -> Option<ReachabilityWarning> {
                None
            }
        }
        let slot = EnvironmentAddressSlot::new();
        assert!(slot.get().is_none());
        assert!(slot.set(Arc::new(Fixed(0))));
        assert!(!slot.set(Arc::new(Fixed(100))));
        assert!(slot.get().is_some());
        assert_eq!(https_url("a.example.com"), "https://a.example.com");
        assert_eq!(https_url("http://a.example.com"), "http://a.example.com");
    }
}
