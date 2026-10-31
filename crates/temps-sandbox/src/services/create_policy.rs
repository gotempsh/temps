// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Operator limits on `POST /v1/sandboxes`.
//!
//! The handler rejects values that can never be valid (non-positive, over the
//! built-in ceilings). What an *operator* allows lives in the `agent_sandbox`
//! settings (`max_cpu_limit`, `max_memory_limit_mb`, `max_pids_limit`,
//! `max_sandboxes_per_user`) and in the memory of the host the sandbox lands
//! on, so it is checked here, by the service, with the node already chosen.
//!
//! Only the HTTP create path asks for these checks
//! (`CreateSandboxRequest::enforce_operator_limits`): AI application
//! workspaces and agent runs are created by the platform itself with limits
//! the platform chose.

use std::sync::OnceLock;

use temps_ai_agents::sandbox::DefaultResourceLimits;
use temps_core::AgentSandboxSettings;

use crate::error::SandboxError;
use crate::services::sandbox_service::CreateSandboxRequest;

/// Memory (MB) of the machine Temps runs on: the smaller of its physical
/// memory and the cgroup limit it runs under, if any. `None` when the OS
/// reports none. Read once: neither changes while the process runs.
pub fn control_plane_memory_mb() -> Option<u64> {
    static MEMORY_MB: OnceLock<Option<u64>> = OnceLock::new();
    *MEMORY_MB.get_or_init(|| {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        let physical = system.total_memory();
        let limit = system
            .cgroup_limits()
            .map(|cgroup| cgroup.total_memory)
            .filter(|bytes| *bytes > 0)
            .map_or(physical, |cgroup| cgroup.min(physical));
        (limit > 0).then_some(limit / (1024 * 1024))
    })
}

/// Bring the limits a request leaves out under the operator maximums.
///
/// An omitted limit becomes whatever the provider defaults to, which can sit
/// above a maximum the operator set. Where the provider's default is known
/// and higher than the maximum, the request is pinned to the maximum. A
/// default that is unknown is left alone: pinning it would turn a ceiling
/// into the size of every default sandbox, and the providers that run
/// sandboxes report their defaults. Limits the caller set are left to
/// [`check_requested_limits`].
pub fn clamp_omitted_limits(
    req: &mut CreateSandboxRequest,
    settings: &AgentSandboxSettings,
    defaults: DefaultResourceLimits,
) {
    if req.cpu_limit.is_none() {
        if let (Some(max), Some(default)) = (settings.max_cpu_limit, defaults.cpu_limit) {
            // A whole-core backend would round a fractional pin back up, so
            // pin to the whole cores the maximum allows. A maximum under one
            // core leaves nothing to pin to; the check refuses it.
            let pin = if defaults.cpu_whole_cores {
                max.floor()
            } else {
                max
            };
            if default > max && (!defaults.cpu_whole_cores || pin >= 1.0) {
                req.cpu_limit = Some(pin);
            }
        }
    }
    if req.memory_limit_mb.is_none() {
        if let (Some(max), Some(default)) = (settings.max_memory_limit_mb, defaults.memory_limit_mb)
        {
            if default > max {
                req.memory_limit_mb = Some(max);
            }
        }
    }
    if req.pids_limit.is_none() {
        if let (Some(max), Some(default)) = (settings.max_pids_limit, defaults.pids_limit) {
            if default > max {
                req.pids_limit = Some(max);
            }
        }
    }
}

/// Refuse a request whose own `memory_limit_mb` is more than the machine the
/// sandbox will run on has. `host_memory_mb` is that machine's memory when it
/// is known (the control plane; a worker reports no figure). Run it on the
/// request as the caller sent it: a value filled in from an operator maximum
/// is the operator's choice, not a request for more than the host has.
pub fn check_host_memory(
    req: &CreateSandboxRequest,
    host_memory_mb: Option<u64>,
) -> Result<(), SandboxError> {
    if let (Some(memory), Some(host)) = (req.memory_limit_mb, host_memory_mb) {
        if memory > host {
            return Err(SandboxError::Validation {
                message: format!(
                    "memory_limit_mb/memory {memory} exceeds the {host} MB of memory on the \
                     host this sandbox would run on"
                ),
            });
        }
    }
    Ok(())
}

/// Keep the memory a sandbox ends up with within the machine it runs on.
///
/// A request that leaves `memory_limit_mb` out gets the provider's default,
/// or the operator maximum [`clamp_omitted_limits`] pinned it to; either can
/// be larger than the host has, so it is capped at the host's memory.
/// Memory the caller sent was already checked by [`check_host_memory`], so
/// this only ever changes a value the caller did not choose. A no-op when
/// the host's memory is unknown (a worker reports no figure).
pub fn pin_omitted_memory_to_host(
    req: &mut CreateSandboxRequest,
    defaults: DefaultResourceLimits,
    host_memory_mb: Option<u64>,
) {
    let Some(host) = host_memory_mb else {
        return;
    };
    if req.memory_limit_mb.or(defaults.memory_limit_mb) > Some(host) {
        req.memory_limit_mb = Some(host);
    }
}

/// Refuse a request that asks for more than the operator allows.
///
/// `defaults` are what the backend applies to a limit the request leaves out
/// (run [`clamp_omitted_limits`] first).
pub fn check_requested_limits(
    req: &CreateSandboxRequest,
    settings: &AgentSandboxSettings,
    defaults: DefaultResourceLimits,
) -> Result<(), SandboxError> {
    if let (Some(cpu), Some(max)) = (req.cpu_limit.or(defaults.cpu_limit), settings.max_cpu_limit) {
        // A whole-core backend runs `ceil(cpu)` vCPUs, at least one: that,
        // not the fractional request, is what the maximum has to bound.
        let effective = if defaults.cpu_whole_cores {
            cpu.ceil().max(1.0)
        } else {
            cpu
        };
        if effective > max {
            let note = if defaults.cpu_whole_cores {
                format!(" (this backend rounds {cpu} up to {effective} whole vCPUs, at least 1)")
            } else {
                String::new()
            };
            return Err(SandboxError::Validation {
                message: format!(
                    "cpu_limit/vcpus {cpu} exceeds the maximum of {max} cores an administrator \
                     allows for a sandbox (agent_sandbox.max_cpu_limit){note}"
                ),
            });
        }
    }
    if let Some(memory) = req.memory_limit_mb {
        if let Some(max) = settings.max_memory_limit_mb {
            if memory > max {
                return Err(SandboxError::Validation {
                    message: format!(
                        "memory_limit_mb/memory {memory} exceeds the maximum of {max} MB an \
                         administrator allows for a sandbox (agent_sandbox.max_memory_limit_mb)"
                    ),
                });
            }
        }
    }
    if let (Some(pids), Some(max)) = (req.pids_limit, settings.max_pids_limit) {
        if pids > max {
            return Err(SandboxError::Validation {
                message: format!(
                    "pids_limit {pids} exceeds the maximum of {max} an administrator allows for \
                     a sandbox (agent_sandbox.max_pids_limit)"
                ),
            });
        }
    }
    Ok(())
}

/// Refuse a create that would take `user_id` past `max_sandboxes_per_user`.
/// `live` counts the sandboxes the user already has that are not destroyed.
pub fn check_user_sandbox_cap(
    user_id: i32,
    live: u64,
    settings: &AgentSandboxSettings,
) -> Result<(), SandboxError> {
    match settings.max_sandboxes_per_user {
        Some(limit) if live >= u64::from(limit) => Err(SandboxError::UserSandboxLimitReached {
            user_id,
            limit,
            live,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(cpu: Option<f64>, memory: Option<u64>, pids: Option<i64>) -> CreateSandboxRequest {
        CreateSandboxRequest {
            cpu_limit: cpu,
            memory_limit_mb: memory,
            pids_limit: pids,
            ..Default::default()
        }
    }

    fn settings() -> AgentSandboxSettings {
        AgentSandboxSettings {
            max_cpu_limit: Some(2.0),
            max_memory_limit_mb: Some(4096),
            max_pids_limit: Some(256),
            max_sandboxes_per_user: Some(3),
            ..Default::default()
        }
    }

    fn none() -> DefaultResourceLimits {
        DefaultResourceLimits::default()
    }

    fn vm() -> DefaultResourceLimits {
        DefaultResourceLimits {
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(512),
            pids_limit: None,
            cpu_whole_cores: true,
        }
    }

    fn message(err: SandboxError) -> String {
        match err {
            SandboxError::Validation { message } => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn requests_within_the_operator_maximums_pass() {
        let req = request(Some(2.0), Some(4096), Some(256));
        assert!(check_requested_limits(&req, &settings(), none()).is_ok());
        assert!(check_requested_limits(&request(None, None, None), &settings(), none()).is_ok());
    }

    #[test]
    fn unset_maximums_and_unknown_host_memory_add_no_limit() {
        let req = request(Some(16.0), Some(32_768), Some(4096));
        assert!(check_requested_limits(&req, &AgentSandboxSettings::default(), none()).is_ok());
    }

    #[test]
    fn a_request_over_an_operator_maximum_names_the_setting() {
        let cpu = check_requested_limits(&request(Some(2.5), None, None), &settings(), none())
            .unwrap_err();
        assert!(message(cpu).contains("max_cpu_limit"));
        let memory = check_requested_limits(&request(None, Some(4097), None), &settings(), none())
            .unwrap_err();
        assert!(message(memory).contains("max_memory_limit_mb"));
        let pids = check_requested_limits(&request(None, None, Some(257)), &settings(), none())
            .unwrap_err();
        assert!(message(pids).contains("max_pids_limit"));
    }

    #[test]
    fn memory_above_the_host_is_refused_even_without_an_operator_maximum() {
        let err = check_host_memory(&request(None, Some(16_384), None), Some(2048)).unwrap_err();
        let message = message(err);
        assert!(
            message.contains("16384") && message.contains("2048"),
            "{message}"
        );
        assert!(check_host_memory(&request(None, Some(16_384), None), None).is_ok());
        assert!(check_host_memory(&request(None, None, None), Some(2048)).is_ok());
    }

    #[test]
    fn a_memory_pinned_from_the_operator_maximum_is_not_a_request_for_more_than_the_host() {
        // Host reports 3900 MB, the operator allows up to 4096 and the
        // provider defaults to 8192: a create that names no memory is pinned
        // to 4096 and must still be accepted.
        let defaults = DefaultResourceLimits {
            memory_limit_mb: Some(8192),
            ..Default::default()
        };
        let mut req = request(None, None, None);
        check_host_memory(&req, Some(3900)).expect("nothing requested yet");
        clamp_omitted_limits(&mut req, &settings(), defaults);
        assert_eq!(req.memory_limit_mb, Some(4096));
        assert!(check_requested_limits(&req, &settings(), defaults).is_ok());
    }

    #[test]
    fn omitted_limits_above_an_operator_maximum_are_pinned_to_it() {
        let defaults = DefaultResourceLimits {
            cpu_limit: Some(4.0),
            memory_limit_mb: Some(8192),
            pids_limit: Some(512),
            cpu_whole_cores: false,
        };
        let mut req = request(None, None, None);
        clamp_omitted_limits(&mut req, &settings(), defaults);
        assert_eq!(req.cpu_limit, Some(2.0));
        assert_eq!(req.memory_limit_mb, Some(4096));
        assert_eq!(req.pids_limit, Some(256));
    }

    #[test]
    fn omitted_limits_the_provider_already_keeps_under_the_maximum_stay_unset() {
        let defaults = DefaultResourceLimits {
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(1024),
            pids_limit: Some(128),
            cpu_whole_cores: false,
        };
        let mut req = request(None, None, None);
        clamp_omitted_limits(&mut req, &settings(), defaults);
        assert_eq!(req.cpu_limit, None);
        assert_eq!(req.memory_limit_mb, None);
        assert_eq!(req.pids_limit, None);
    }

    #[test]
    fn unknown_provider_defaults_and_set_values_are_untouched() {
        let mut req = request(Some(1.5), None, None);
        clamp_omitted_limits(&mut req, &settings(), DefaultResourceLimits::default());
        assert_eq!(req.cpu_limit, Some(1.5));
        assert_eq!(req.memory_limit_mb, None);
        assert_eq!(req.pids_limit, None);
        let mut set = request(Some(1.5), Some(100), Some(10));
        clamp_omitted_limits(
            &mut set,
            &settings(),
            DefaultResourceLimits {
                cpu_limit: Some(4.0),
                memory_limit_mb: Some(8192),
                pids_limit: Some(512),
                cpu_whole_cores: false,
            },
        );
        assert_eq!(set.cpu_limit, Some(1.5));
        assert_eq!(set.memory_limit_mb, Some(100));
        assert_eq!(set.pids_limit, Some(10));
    }

    #[test]
    fn a_whole_core_backend_is_bounded_by_the_vcpus_it_will_run() {
        let cap = |max: f64| AgentSandboxSettings {
            max_cpu_limit: Some(max),
            ..Default::default()
        };
        // 1.5 cores is 2 vCPUs: over a maximum of 1.5.
        let err =
            check_requested_limits(&request(Some(1.5), None, None), &cap(1.5), vm()).unwrap_err();
        assert!(message(err).contains("2 whole vCPUs"));
        // 1.2 cores is 2 vCPUs and 1.0 is 1: only the latter fits under 1.5.
        assert!(check_requested_limits(&request(Some(1.0), None, None), &cap(1.5), vm()).is_ok());
        // A fractional request still runs one full vCPU: over a 0.5 maximum.
        assert!(check_requested_limits(&request(Some(0.25), None, None), &cap(0.5), vm()).is_err());
        // The same fractional values are fine on a backend that honors them.
        assert!(
            check_requested_limits(&request(Some(0.25), None, None), &cap(0.5), none()).is_ok()
        );
    }

    #[test]
    fn an_omitted_cpu_on_a_whole_core_backend_is_pinned_to_whole_cores_or_refused() {
        let cap = |max: f64| AgentSandboxSettings {
            max_cpu_limit: Some(max),
            ..Default::default()
        };
        let two_vcpus = DefaultResourceLimits {
            cpu_limit: Some(2.0),
            ..vm()
        };
        // Default 2 vCPUs under a 1.5 maximum: pinned to 1, not to 1.5 (= 2).
        let mut req = request(None, None, None);
        clamp_omitted_limits(&mut req, &cap(1.5), two_vcpus);
        assert_eq!(req.cpu_limit, Some(1.0));
        assert!(check_requested_limits(&req, &cap(1.5), two_vcpus).is_ok());
        // Nothing under one vCPU exists: the create is refused, not run on 1.
        let mut req = request(None, None, None);
        clamp_omitted_limits(&mut req, &cap(0.5), vm());
        assert_eq!(req.cpu_limit, None);
        let err = check_requested_limits(&req, &cap(0.5), vm()).unwrap_err();
        assert!(message(err).contains("max_cpu_limit"));
    }

    #[test]
    fn an_omitted_memory_above_the_host_is_capped_at_the_host() {
        let defaults = DefaultResourceLimits {
            memory_limit_mb: Some(8192),
            ..Default::default()
        };
        // No operator maximum: the provider default is over the host.
        let mut req = request(None, None, None);
        pin_omitted_memory_to_host(&mut req, defaults, Some(3900));
        assert_eq!(req.memory_limit_mb, Some(3900));
        // Operator maximum above the host: pinned to it, then capped.
        let mut req = request(None, None, None);
        clamp_omitted_limits(&mut req, &settings(), defaults);
        pin_omitted_memory_to_host(&mut req, defaults, Some(3000));
        assert_eq!(req.memory_limit_mb, Some(3000));
        // Within the host, or the host unknown: untouched.
        let mut req = request(None, None, None);
        pin_omitted_memory_to_host(&mut req, defaults, Some(16_384));
        assert_eq!(req.memory_limit_mb, None);
        pin_omitted_memory_to_host(&mut req, defaults, None);
        assert_eq!(req.memory_limit_mb, None);
    }

    #[test]
    fn the_user_cap_refuses_at_the_limit_and_not_below_it() {
        assert!(check_user_sandbox_cap(7, 2, &settings()).is_ok());
        let err = check_user_sandbox_cap(7, 3, &settings()).unwrap_err();
        assert!(matches!(
            err,
            SandboxError::UserSandboxLimitReached {
                user_id: 7,
                limit: 3,
                live: 3
            }
        ));
        assert!(check_user_sandbox_cap(7, 1_000, &AgentSandboxSettings::default()).is_ok());
    }

    #[test]
    fn this_machine_reports_some_memory() {
        let memory = control_plane_memory_mb().expect("sysinfo reports memory");
        assert!(memory > 0);
    }
}
