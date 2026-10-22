// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admission (ADR-055 §4): the one gate every spec passes, in this order.
//!
//! 1. defaults for the kind;
//! 2. the security profile, chosen together with the ADR-045 socket grant:
//!    a spec for a granted project gets `HostPrivileged`, a spec asking for
//!    it without the grant is refused, every other spec gets its kind's
//!    profile;
//! 3. image allow-lists;
//! 4. quotas;
//! 5. validation.
//!
//! [`admit`] is pure: the caller supplies the grant, the allow-lists and the
//! number of live workloads the owner already has, so the order is testable
//! without a database.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use temps_core::docker_socket_grant::DockerSocketGrant;
use temps_workload::{
    Artifact, EgressProfile, Isolation, Lifetime, SecurityProfile, WorkloadKind, WorkloadSpec,
};

use crate::error::AdmissionError;
use crate::plane::Owner;

/// `RunToCompletion { timeout_secs: 0 }` for a job takes this maximum run
/// time: one hour.
pub const DEFAULT_JOB_TIMEOUT_SECS: u32 = 3_600;
/// A finished job run is removed this long after it finished unless its
/// spec says otherwise: ten minutes, long enough to read its logs.
pub const DEFAULT_JOB_TTL_SECS: u32 = 600;
/// Longest workload name: a DNS label, so it can become a container name.
pub const MAX_NAME_LEN: usize = 63;

/// The rule that refused a spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionRule {
    KindDefaults,
    SecurityProfile,
    ImageAllowList,
    Quota,
    Validation,
}

impl AdmissionRule {
    pub fn as_str(self) -> &'static str {
        match self {
            AdmissionRule::KindDefaults => "kind_defaults",
            AdmissionRule::SecurityProfile => "security_profile",
            AdmissionRule::ImageAllowList => "image_allow_list",
            AdmissionRule::Quota => "quota",
            AdmissionRule::Validation => "validation",
        }
    }
}

impl fmt::Display for AdmissionRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Images a kind may run, matched exactly (`repository:tag` or
/// `repository@sha256:…`). A kind without a list runs any image: a job runs
/// its project's own deployment image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageAllowList {
    pub images: Vec<String>,
}

impl ImageAllowList {
    pub fn allows(&self, reference: &str) -> bool {
        self.images.iter().any(|image| image == reference)
    }
}

/// Per-owner limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quotas {
    /// Unfinished workloads one owner may have at once. Bounds a schedule
    /// with `allow` concurrency whose runs never finish.
    pub max_active_per_owner: u32,
    /// Longest maximum run time a run-to-completion workload may ask for.
    pub max_timeout_secs: u32,
}

impl Default for Quotas {
    fn default() -> Self {
        Self {
            max_active_per_owner: 5,
            max_timeout_secs: 86_400,
        }
    }
}

/// What admission is configured with. Built once at startup.
#[derive(Debug, Clone, Default)]
pub struct AdmissionPolicy {
    /// The ADR-045 grant of this host (`process_grant()` in `temps serve`).
    pub grant: DockerSocketGrant,
    pub image_allow_lists: BTreeMap<WorkloadKind, ImageAllowList>,
    pub quotas: Quotas,
}

/// One spec to admit.
#[derive(Debug, Clone)]
pub struct AdmissionInput<'a> {
    pub spec: WorkloadSpec,
    pub name: &'a str,
    pub owner: &'a Owner,
    /// The slug of the project the workload runs for, if any. Only a project
    /// can hold the socket grant.
    pub project_slug: Option<&'a str>,
    /// Unfinished workloads the owner has, not counting one of this name.
    pub active_for_owner: u32,
}

/// The kind's own security profile (§4 table).
pub fn kind_profile(kind: WorkloadKind) -> SecurityProfile {
    match kind {
        WorkloadKind::Deployment | WorkloadKind::Job => SecurityProfile::Trusted,
        WorkloadKind::Service => SecurityProfile::Data,
        WorkloadKind::Sandbox | WorkloadKind::Function => SecurityProfile::Untrusted,
    }
}

fn kind_egress(kind: WorkloadKind) -> EgressProfile {
    match kind {
        WorkloadKind::Deployment | WorkloadKind::Job | WorkloadKind::Service => EgressProfile::Open,
        WorkloadKind::Sandbox | WorkloadKind::Function => EgressProfile::Restricted,
    }
}

/// Admit `input.spec`, returning the spec as it will be stored: defaults
/// filled and the security profile set.
pub fn admit(
    policy: &AdmissionPolicy,
    input: AdmissionInput<'_>,
) -> Result<WorkloadSpec, AdmissionError> {
    let AdmissionInput {
        mut spec,
        name,
        owner,
        project_slug,
        active_for_owner,
    } = input;
    let refuse = |rule: AdmissionRule, message: String| AdmissionError {
        rule,
        workload: name.to_string(),
        owner: owner.to_string(),
        message,
    };
    let kind = spec.owner.kind;

    // 1. Defaults for the kind.
    if spec.network.egress.is_none() {
        spec.network.egress = Some(kind_egress(kind));
    }
    if let Lifetime::RunToCompletion {
        timeout_secs,
        ttl_secs_after_finished,
        ..
    } = &mut spec.lifetime
    {
        if *timeout_secs == 0 {
            *timeout_secs = DEFAULT_JOB_TIMEOUT_SECS;
        }
        if ttl_secs_after_finished.is_none() {
            *ttl_secs_after_finished = Some(DEFAULT_JOB_TTL_SECS);
        }
    }

    // 2. The security profile and the socket grant, decided together.
    let granted = project_slug.is_some_and(|slug| policy.grant.allows(slug))
        && matches!(kind, WorkloadKind::Deployment | WorkloadKind::Job);
    let profile = if granted {
        SecurityProfile::HostPrivileged
    } else if spec.security == Some(SecurityProfile::HostPrivileged) {
        return Err(refuse(
            AdmissionRule::SecurityProfile,
            format!(
                "asks for host_privileged, but project '{}' does not hold the ADR-045 \
                 Docker socket grant on this host",
                project_slug.unwrap_or("<none>")
            ),
        ));
    } else {
        kind_profile(kind)
    };
    spec.security = Some(profile);
    if profile == SecurityProfile::Untrusted && spec.network.egress == Some(EgressProfile::Open) {
        // Untrusted workloads get at most `restricted` egress.
        spec.network.egress = Some(EgressProfile::Restricted);
    }

    // 3. Image allow-lists.
    if let (Some(list), Artifact::OciImage { reference, .. }) =
        (policy.image_allow_lists.get(&kind), &spec.artifact)
    {
        if !list.allows(reference) {
            return Err(refuse(
                AdmissionRule::ImageAllowList,
                format!(
                    "image '{reference}' is not on the {kind} allow-list ({} entries)",
                    list.images.len()
                ),
            ));
        }
    }

    // 4. Quotas.
    if active_for_owner >= policy.quotas.max_active_per_owner {
        return Err(refuse(
            AdmissionRule::Quota,
            format!(
                "{owner} already has {active_for_owner} unfinished workloads; the limit is {}",
                policy.quotas.max_active_per_owner
            ),
        ));
    }
    if let Lifetime::RunToCompletion { timeout_secs, .. } = spec.lifetime {
        if timeout_secs > policy.quotas.max_timeout_secs {
            return Err(refuse(
                AdmissionRule::Quota,
                format!(
                    "maximum run time {timeout_secs}s exceeds the limit of {}s",
                    policy.quotas.max_timeout_secs
                ),
            ));
        }
    }

    // 5. Validation.
    validate(&spec, name).map_err(|message| refuse(AdmissionRule::Validation, message))?;
    Ok(spec)
}

/// What phase 2 can run: run-to-completion jobs, in a container on the
/// control plane, with nothing the Docker driver cannot deliver yet.
fn validate(spec: &WorkloadSpec, name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!(
            "name must be 1 to {MAX_NAME_LEN} characters, got {}",
            name.len()
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return Err(format!(
            "name '{name}' must be lowercase letters, digits and inner hyphens"
        ));
    }
    if spec.owner.kind != WorkloadKind::Job {
        return Err(format!(
            "kind '{}' is not run by the plane yet; phase 2 runs only jobs",
            spec.owner.kind
        ));
    }
    if !matches!(spec.lifetime, Lifetime::RunToCompletion { .. }) {
        return Err("a job must have a run_to_completion lifetime".to_string());
    }
    match &spec.artifact {
        Artifact::OciImage { reference, .. } if reference.trim().is_empty() => {
            return Err("the OCI image reference is empty".to_string());
        }
        Artifact::OciImage { .. } => {}
        other => {
            return Err(format!(
                "artifact {other:?} needs a driver this node does not have; only OCI images run"
            ));
        }
    }
    if spec.isolation != Isolation::Process {
        return Err(format!(
            "isolation {:?} is not available; only process isolation runs",
            spec.isolation
        ));
    }
    if let Some(command) = &spec.process.command {
        if command.is_empty() {
            return Err("command is empty; omit it to keep the image's".to_string());
        }
        if command.iter().any(|arg| arg.contains('\0')) {
            return Err("command contains a NUL byte".to_string());
        }
    }
    if !spec.env.secrets.is_empty() {
        return Err(format!(
            "{} secret reference(s) need secret-file delivery, which arrives with the node \
             runtime; pass the environment as an import instead",
            spec.env.secrets.len()
        ));
    }
    if !spec.volumes.is_empty() {
        return Err(format!(
            "{} volume mount(s) requested; jobs cannot mount volumes yet",
            spec.volumes.len()
        ));
    }
    if !spec.ports.is_empty() {
        return Err(format!(
            "{} port(s) requested; a job publishes no ports",
            spec.ports.len()
        ));
    }
    match spec.network.egress {
        Some(EgressProfile::Open) | None => {}
        Some(other) => {
            return Err(format!(
                "egress profile {other:?} cannot be enforced yet; only open egress runs"
            ));
        }
    }
    if spec.placement.replicas != 1 {
        return Err(format!(
            "a job runs one instance, got replicas {}",
            spec.placement.replicas
        ));
    }
    if let Some(nodes) = &spec.placement.nodes {
        if nodes.iter().any(|node| *node != 0) {
            return Err(format!(
                "placement on nodes {nodes:?} is not available; jobs run on the control plane"
            ));
        }
    }
    if let Some(label) = spec
        .labels
        .keys()
        .find(|key| crate::labels::RESERVED.contains(&key.as_str()))
    {
        return Err(format!(
            "label '{label}' is set by the plane, not the product"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::job_spec;

    fn owner() -> Owner {
        Owner::new("cron", 7)
    }

    fn input<'a>(
        spec: WorkloadSpec,
        owner: &'a Owner,
        slug: Option<&'a str>,
    ) -> AdmissionInput<'a> {
        AdmissionInput {
            spec,
            name: "cron-7-1700000000",
            owner,
            project_slug: slug,
            active_for_owner: 0,
        }
    }

    #[test]
    fn kind_defaults_fill_the_timeout_ttl_egress_and_profile() {
        let owner = owner();
        let mut spec = job_spec("app:1");
        spec.lifetime = Lifetime::RunToCompletion {
            timeout_secs: 0,
            retry_limit: 1,
            ttl_secs_after_finished: None,
        };
        let admitted = admit(
            &AdmissionPolicy::default(),
            input(spec, &owner, Some("shop")),
        )
        .expect("admitted");
        assert_eq!(
            admitted.lifetime,
            Lifetime::RunToCompletion {
                timeout_secs: DEFAULT_JOB_TIMEOUT_SECS,
                retry_limit: 1,
                ttl_secs_after_finished: Some(DEFAULT_JOB_TTL_SECS),
            }
        );
        assert_eq!(admitted.network.egress, Some(EgressProfile::Open));
        assert_eq!(admitted.security, Some(SecurityProfile::Trusted));
    }

    #[test]
    fn a_product_cannot_choose_its_own_profile() {
        let owner = owner();
        let mut spec = job_spec("app:1");
        spec.security = Some(SecurityProfile::Untrusted);
        let admitted =
            admit(&AdmissionPolicy::default(), input(spec, &owner, None)).expect("admitted");
        assert_eq!(admitted.security, Some(SecurityProfile::Trusted));
    }

    #[test]
    fn a_granted_project_gets_host_privileged_and_others_are_refused_it() {
        let owner = owner();
        let policy = AdmissionPolicy {
            grant: DockerSocketGrant::from_slugs(["infra-agent"]),
            ..Default::default()
        };
        let granted = admit(
            &policy,
            input(job_spec("app:1"), &owner, Some("infra-agent")),
        )
        .expect("admitted");
        assert_eq!(granted.security, Some(SecurityProfile::HostPrivileged));

        let ungranted =
            admit(&policy, input(job_spec("app:1"), &owner, Some("shop"))).expect("admitted");
        assert_eq!(ungranted.security, Some(SecurityProfile::Trusted));

        let mut asks = job_spec("app:1");
        asks.security = Some(SecurityProfile::HostPrivileged);
        let refused = admit(&policy, input(asks, &owner, Some("shop"))).unwrap_err();
        assert_eq!(refused.rule, AdmissionRule::SecurityProfile);
        assert!(refused.message.contains("'shop'"), "{}", refused.message);
        assert_eq!(refused.workload, "cron-7-1700000000");
        assert_eq!(refused.owner, "cron/7");
    }

    /// A spec that breaks every rule is refused by the first one in §4's
    /// order; fixing that one surfaces the next.
    #[test]
    fn rules_run_in_the_adr_order() {
        let owner = owner();
        let mut policy = AdmissionPolicy {
            quotas: Quotas {
                max_active_per_owner: 1,
                max_timeout_secs: 60,
            },
            ..Default::default()
        };
        policy.image_allow_lists.insert(
            WorkloadKind::Job,
            ImageAllowList {
                images: vec!["allowed:1".into()],
            },
        );
        let mut spec = job_spec("not-allowed:1");
        spec.security = Some(SecurityProfile::HostPrivileged);
        spec.ports.push(temps_workload::PortSpec {
            name: "http".into(),
            container_port: 80,
            protocol: Default::default(),
            app_protocol: Default::default(),
        });
        let mut all_broken = input(spec.clone(), &owner, None);
        all_broken.active_for_owner = 3;

        let order: Vec<AdmissionRule> = {
            let mut seen = Vec::new();
            let mut attempt = all_broken;
            loop {
                match admit(&policy, attempt.clone()) {
                    Ok(_) => break,
                    Err(error) => {
                        seen.push(error.rule);
                        match error.rule {
                            AdmissionRule::SecurityProfile => attempt.spec.security = None,
                            AdmissionRule::ImageAllowList => {
                                attempt.spec.artifact = Artifact::oci("allowed:1")
                            }
                            AdmissionRule::Quota if attempt.active_for_owner > 0 => {
                                attempt.active_for_owner = 0
                            }
                            AdmissionRule::Quota => {
                                attempt.spec.lifetime = Lifetime::RunToCompletion {
                                    timeout_secs: 30,
                                    retry_limit: 0,
                                    ttl_secs_after_finished: None,
                                }
                            }
                            AdmissionRule::Validation => attempt.spec.ports.clear(),
                            AdmissionRule::KindDefaults => unreachable!("defaults never refuse"),
                        }
                    }
                }
            }
            seen
        };
        assert_eq!(
            order,
            vec![
                AdmissionRule::SecurityProfile,
                AdmissionRule::ImageAllowList,
                AdmissionRule::Quota,
                AdmissionRule::Quota,
                AdmissionRule::Validation,
            ]
        );
    }

    #[test]
    fn validation_refuses_what_phase_two_cannot_run() {
        let owner = owner();
        let policy = AdmissionPolicy::default();
        type Mutation = Box<dyn Fn(&mut WorkloadSpec)>;
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "long_running",
                Box::new(|s| {
                    s.lifetime = Lifetime::LongRunning {
                        restart: Default::default(),
                    }
                }),
            ),
            (
                "secret",
                Box::new(|s| {
                    s.env.secrets.push(temps_workload::SecretRef {
                        name: "API_TOKEN".into(),
                        source: "env_var:1".into(),
                    })
                }),
            ),
            (
                "restricted egress",
                Box::new(|s| s.network.egress = Some(EgressProfile::Restricted)),
            ),
            (
                "empty command",
                Box::new(|s| s.process.command = Some(vec![])),
            ),
            (
                "reserved label",
                Box::new(|s| {
                    s.labels
                        .insert(temps_workload::labels::WORKLOAD_ID.into(), "1".into());
                }),
            ),
            (
                "worker node",
                Box::new(|s| s.placement.nodes = Some(vec![3])),
            ),
        ];
        for (case, mutate) in cases {
            let mut spec = job_spec("app:1");
            mutate(&mut spec);
            let error = admit(&policy, input(spec, &owner, None))
                .expect_err(&format!("{case} must be refused"));
            assert_eq!(error.rule, AdmissionRule::Validation, "{case}: {error}");
        }
        let mut bad_name = input(job_spec("app:1"), &owner, None);
        bad_name.name = "Cron_7";
        assert_eq!(
            admit(&policy, bad_name).unwrap_err().rule,
            AdmissionRule::Validation
        );
    }
}
