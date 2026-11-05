// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Inheritance, validation and ceiling checks for deployment configuration.
//!
//! The types themselves stay in `temps_entities::deployment_config` because
//! entity models store them as typed JSON columns, so these are extension
//! traits: calls keep their form, and a caller that loses the import stops
//! compiling instead of skipping a check.

use temps_entities::deployment_config::{
    BuildLocation, DeploymentConfig, DeploymentConfigSnapshot, RateLimitConfig, SecurityConfig,
    SecurityHeadersConfig, DEFAULT_HEALTH_CHECK_TIMEOUT_SECONDS, DEFAULT_STOP_TIMEOUT_SECONDS,
    MAX_HEALTH_CHECK_TIMEOUT_SECONDS, MAX_REPLICAS, MAX_STOP_TIMEOUT_SECONDS,
    MIN_HEALTH_CHECK_TIMEOUT_SECONDS, MIN_STOP_TIMEOUT_SECONDS,
};
use temps_entities::environments;

/// Smallest non-zero CPU limit Docker accepts: 0.01 cores, in microcores.
pub const MIN_CPU_LIMIT_MICROCORES: i32 = 10_000;

/// Effective readiness budget in seconds for a resolved override: clamped to
/// the accepted range, or the platform default when unset. Stored values are
/// validated on write; the clamp keeps a hand-edited row from disabling the
/// health gate (0) or parking a deployment for days.
pub fn resolve_health_check_timeout_seconds(configured: Option<i32>) -> u64 {
    let seconds = configured
        .map(|s| {
            s.clamp(
                MIN_HEALTH_CHECK_TIMEOUT_SECONDS,
                MAX_HEALTH_CHECK_TIMEOUT_SECONDS,
            )
        })
        .unwrap_or(DEFAULT_HEALTH_CHECK_TIMEOUT_SECONDS);
    u64::from(seconds.unsigned_abs())
}

/// Effective stop grace period in seconds for a resolved override: clamped to
/// the accepted range, or the platform default (10 s) when unset. Stored
/// values are validated on write; the clamp keeps a hand-edited row from
/// killing containers with no grace at all (0) or holding a teardown for days.
pub fn resolve_stop_timeout_seconds(configured: Option<i32>) -> u64 {
    let seconds = configured
        .map(|s| s.clamp(MIN_STOP_TIMEOUT_SECONDS, MAX_STOP_TIMEOUT_SECONDS))
        .unwrap_or(DEFAULT_STOP_TIMEOUT_SECONDS);
    u64::from(seconds.unsigned_abs())
}

/// Refuse a stop grace period outside the accepted range, wherever one is saved.
pub fn validate_stop_timeout_seconds(seconds: i32) -> Result<(), String> {
    if (MIN_STOP_TIMEOUT_SECONDS..=MAX_STOP_TIMEOUT_SECONDS).contains(&seconds) {
        Ok(())
    } else {
        Err(format!(
            "Stop timeout {seconds} is not in valid range \
             ({MIN_STOP_TIMEOUT_SECONDS}-{MAX_STOP_TIMEOUT_SECONDS} seconds)"
        ))
    }
}

/// Refuse a replica count no deployment could run, wherever one is saved.
pub fn validate_replicas(replicas: i32) -> Result<(), String> {
    if replicas < 1 {
        return Err(format!("Replicas must be at least 1, got {replicas}"));
    }
    if replicas > MAX_REPLICAS {
        return Err(format!(
            "Replicas cannot exceed {MAX_REPLICAS}, got {replicas}"
        ));
    }
    Ok(())
}

pub trait SecurityConfigExt {
    /// Merge this config with another, preferring values from `other`
    ///
    /// This is used for inheritance chain: Environment > Project > Global
    /// When resolving effective security config, call: global.merge(&project).merge(&environment)
    fn merge(&self, other: &SecurityConfig) -> SecurityConfig;
}

impl SecurityConfigExt for SecurityConfig {
    fn merge(&self, other: &SecurityConfig) -> SecurityConfig {
        SecurityConfig {
            enabled: other.enabled.or(self.enabled),
            headers: match (&self.headers, &other.headers) {
                (Some(base), Some(override_headers)) => Some(merge_headers(base, override_headers)),
                (Some(base), None) => Some(base.clone()),
                (None, Some(override_headers)) => Some(override_headers.clone()),
                (None, None) => None,
            },
            rate_limiting: match (&self.rate_limiting, &other.rate_limiting) {
                (Some(base), Some(override_rl)) => Some(merge_rate_limit(base, override_rl)),
                (Some(base), None) => Some(base.clone()),
                (None, Some(override_rl)) => Some(override_rl.clone()),
                (None, None) => None,
            },
            attack_mode: other
                .attack_mode
                .clone()
                .or_else(|| self.attack_mode.clone()),
            challenge_config: other
                .challenge_config
                .clone()
                .or_else(|| self.challenge_config.clone()),
            geo_restrictions: other
                .geo_restrictions
                .clone()
                .or_else(|| self.geo_restrictions.clone()),
            password_protection: other
                .password_protection
                .clone()
                .or_else(|| self.password_protection.clone()),
        }
    }
}

/// Merge headers config, preferring values from `other`
fn merge_headers(
    base: &SecurityHeadersConfig,
    other: &SecurityHeadersConfig,
) -> SecurityHeadersConfig {
    SecurityHeadersConfig {
        preset: other.preset.clone().or_else(|| base.preset.clone()),
        content_security_policy: other
            .content_security_policy
            .clone()
            .or_else(|| base.content_security_policy.clone()),
        x_frame_options: other
            .x_frame_options
            .clone()
            .or_else(|| base.x_frame_options.clone()),
        strict_transport_security: other
            .strict_transport_security
            .clone()
            .or_else(|| base.strict_transport_security.clone()),
        referrer_policy: other
            .referrer_policy
            .clone()
            .or_else(|| base.referrer_policy.clone()),
    }
}

/// Merge rate limit config, preferring values from `other`
fn merge_rate_limit(base: &RateLimitConfig, other: &RateLimitConfig) -> RateLimitConfig {
    let mut merged_whitelist = base.whitelist_ips.clone();
    merged_whitelist.extend(other.whitelist_ips.clone());
    merged_whitelist.sort();
    merged_whitelist.dedup();

    let mut merged_blacklist = base.blacklist_ips.clone();
    merged_blacklist.extend(other.blacklist_ips.clone());
    merged_blacklist.sort();
    merged_blacklist.dedup();

    RateLimitConfig {
        max_requests_per_minute: other
            .max_requests_per_minute
            .or(base.max_requests_per_minute),
        max_requests_per_hour: other.max_requests_per_hour.or(base.max_requests_per_hour),
        whitelist_ips: merged_whitelist,
        blacklist_ips: merged_blacklist,
    }
}

pub trait DeploymentConfigExt {
    /// Return a node selector only when it names at least one node.
    ///
    /// Empty lists are the API sentinel for clearing an environment override.
    /// Treating a legacy stored empty list as an override would either make the
    /// workload unschedulable or erase a project-level placement constraint.
    fn configured_target_nodes(&self) -> Option<&[i32]>;

    /// Return a label selector only when it contains at least one label.
    /// Empty objects are the API sentinel for clearing an environment override.
    fn configured_target_labels(&self) -> Option<&serde_json::Value>;

    /// The effective build location for an environment of a project: the
    /// environment's setting, else the project's, else the control plane.
    fn resolve_build_location(
        environment: Option<&DeploymentConfig>,
        project: Option<&DeploymentConfig>,
    ) -> BuildLocation;

    /// Merge this config with another, preferring values from `other`
    ///
    /// This is useful for merging environment-level config (other) with
    /// project-level config (self) to get the effective configuration.
    fn merge(&self, other: &DeploymentConfig) -> DeploymentConfig;

    /// Validate the resource configuration
    fn validate(&self) -> Result<(), String>;

    /// Check this config against the operator's tenant resource ceilings.
    ///
    /// Three of the per-project overrides above use `Some(0)` as an explicit
    /// "unlimited" sentinel that beats the instance-wide default: memory limit,
    /// concurrent-connection cap, and the request/SSE/WebSocket timeouts. That
    /// is deliberate — a dedicated workload sometimes needs it — but on a
    /// shared instance it also means anyone who can edit a project's config can
    /// opt that project out of the operator's global guardrails.
    ///
    /// Ceilings close that off *only when an operator has set them*. Every
    /// field defaults to unenforced, so an upgrade changes nothing until the
    /// operator opts in. Violations are **rejected with a reason**, never
    /// silently clamped, so the user sees why their value did not take.
    ///
    /// Returns every violation rather than the first, so a form with two bad
    /// fields does not need two round trips to fix.
    fn check_against_tenant_ceilings(
        &self,
        ceilings: &crate::TenantResourceCeilings,
    ) -> Result<(), Vec<String>>;

    /// Effective readiness budget in seconds for this config alone.
    fn effective_health_check_timeout_seconds(&self) -> u64;
}

impl DeploymentConfigExt for DeploymentConfig {
    fn configured_target_nodes(&self) -> Option<&[i32]> {
        self.target_nodes
            .as_deref()
            .filter(|nodes| !nodes.is_empty())
    }

    fn configured_target_labels(&self) -> Option<&serde_json::Value> {
        self.target_labels
            .as_ref()
            .filter(|labels| !labels.as_object().is_some_and(serde_json::Map::is_empty))
    }

    fn resolve_build_location(
        environment: Option<&DeploymentConfig>,
        project: Option<&DeploymentConfig>,
    ) -> BuildLocation {
        environment
            .and_then(|config| config.build_location)
            .or_else(|| project.and_then(|config| config.build_location))
            .unwrap_or(BuildLocation::ControlPlane)
    }

    fn merge(&self, other: &DeploymentConfig) -> DeploymentConfig {
        DeploymentConfig {
            cpu_request: other.cpu_request.or(self.cpu_request),
            cpu_limit: other.cpu_limit.or(self.cpu_limit),
            memory_request: other.memory_request.or(self.memory_request),
            memory_limit: other.memory_limit.or(self.memory_limit),
            exposed_port: other.exposed_port.or(self.exposed_port),
            // Env-wins semantics: if the environment has an explicit value use it,
            // otherwise inherit the project-level setting.
            automatic_deploy: other.automatic_deploy.or(self.automatic_deploy),
            performance_metrics_enabled: other.performance_metrics_enabled
                || self.performance_metrics_enabled,
            session_recording_enabled: other.session_recording_enabled
                || self.session_recording_enabled,
            container_exec_enabled: other.container_exec_enabled || self.container_exec_enabled,
            // Use other's replicas if > 0, otherwise use self's replicas
            replicas: if other.replicas > 0 {
                other.replicas
            } else {
                self.replicas
            },
            // Merge security configurations
            security: match (&self.security, &other.security) {
                (Some(base), Some(override_security)) => Some(base.merge(override_security)),
                (Some(base), None) => Some(base.clone()),
                (None, Some(override_security)) => Some(override_security.clone()),
                (None, None) => None,
            },
            // A non-empty environment selector overrides the project. Empty
            // selectors mean "clear this override", so inherit the project
            // selector instead of erasing its placement boundary.
            target_nodes: other
                .configured_target_nodes()
                .map(<[i32]>::to_vec)
                .or_else(|| self.configured_target_nodes().map(<[i32]>::to_vec)),
            target_labels: other
                .configured_target_labels()
                .cloned()
                .or_else(|| self.configured_target_labels().cloned()),
            anti_affinity: other.anti_affinity,
            // Env-wins, inheriting the project when unset — same semantics as
            // `automatic_deploy`. Deliberately not `||`: an environment must be
            // able to turn cross-builds *off* for a project that has them on,
            // which an OR would make impossible.
            cross_architecture_builds: other
                .cross_architecture_builds
                .or(self.cross_architecture_builds),
            // Env-wins, inheriting the project when unset: production can pin
            // the control plane while previews of the same project build on
            // nodes, or the other way round.
            build_location: other.build_location.or(self.build_location),
            on_demand: other.on_demand || self.on_demand,
            idle_timeout_seconds: if other.idle_timeout_seconds != 300 {
                other.idle_timeout_seconds
            } else {
                self.idle_timeout_seconds
            },
            wake_timeout_seconds: if other.wake_timeout_seconds != 30 {
                other.wake_timeout_seconds
            } else {
                self.wake_timeout_seconds
            },
            request_timeout_seconds: other
                .request_timeout_seconds
                .or(self.request_timeout_seconds),
            sse_idle_timeout_seconds: other
                .sse_idle_timeout_seconds
                .or(self.sse_idle_timeout_seconds),
            websocket_idle_timeout_seconds: other
                .websocket_idle_timeout_seconds
                .or(self.websocket_idle_timeout_seconds),
            max_concurrent_connections: other
                .max_concurrent_connections
                .or(self.max_concurrent_connections),
            health: other.health.clone().or_else(|| self.health.clone()),
            health_check_timeout_seconds: other
                .health_check_timeout_seconds
                .or(self.health_check_timeout_seconds),
            stop_timeout_seconds: other.stop_timeout_seconds.or(self.stop_timeout_seconds),
        }
    }

    fn validate(&self) -> Result<(), String> {
        // CPU request should not exceed CPU limit. A limit of 0 is the explicit
        // "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.cpu_request, self.cpu_limit) {
            if limit != 0 && request > limit {
                return Err("CPU request cannot exceed CPU limit".to_string());
            }
        }

        // Negative values are never meaningful here, and for `memory_limit` a
        // negative is actively dangerous: `parse_memory_mb` rejects it and the
        // deployer then leaves Docker's memory cgroup unset, so `-1` runs the
        // container *uncapped* while reading like a tighter limit than `0`.
        for (name, value) in [
            ("CPU request", self.cpu_request),
            ("CPU limit", self.cpu_limit),
            ("Memory request", self.memory_request),
            ("Memory limit", self.memory_limit),
        ] {
            if let Some(value) = value {
                if value < 0 {
                    return Err(format!(
                        "{name} cannot be negative, got {value} (use 0 for unlimited)"
                    ));
                }
            }
        }

        // Docker rejects a CPU limit below 0.01 cores, so a smaller one saves
        // fine and then fails every deployment when the container won't start.
        // Usually a unit mistake: a millicore value sent without converting it
        // to microcores. Requests are not applied to containers, so only the
        // limit needs the floor.
        if let Some(limit) = self.cpu_limit {
            if limit > 0 && limit < MIN_CPU_LIMIT_MICROCORES {
                return Err(format!(
                    "CPU limit must be 0 (uncapped) or at least {MIN_CPU_LIMIT_MICROCORES} microcores \
                     (0.01 cores), got {limit}. CPU is in microcores: 1000000 = 1 core"
                ));
            }
        }

        // Memory request should not exceed memory limit. A limit of 0 is the
        // explicit "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.memory_request, self.memory_limit) {
            if limit != 0 && request > limit {
                return Err("Memory request cannot exceed memory limit".to_string());
            }
        }

        validate_replicas(self.replicas)?;

        // Port should be in valid range
        if let Some(port) = self.exposed_port {
            if !(1..=65535).contains(&port) {
                return Err(format!("Port {} is not in valid range (1-65535)", port));
            }
        }

        // Idle timeout should be in valid range when on-demand is enabled
        if self.on_demand {
            if !(60..=86400).contains(&self.idle_timeout_seconds) {
                return Err(format!(
                    "Idle timeout {} is not in valid range (60-86400 seconds)",
                    self.idle_timeout_seconds
                ));
            }
            if !(5..=120).contains(&self.wake_timeout_seconds) {
                return Err(format!(
                    "Wake timeout {} is not in valid range (5-120 seconds)",
                    self.wake_timeout_seconds
                ));
            }
        }

        // Sanity bounds only — the operator's hard ceiling
        // (`AppSettings.request_timeouts.max_request_timeout_seconds`) is
        // enforced separately at resolution time, since this type has no
        // access to global settings. `0` is a valid explicit "no timeout"
        // override (same uncapped-sentinel convention as `cpu_limit`/
        // `memory_limit` above), so the floor is 0, not 1.
        for (name, value) in [
            ("Request timeout", self.request_timeout_seconds),
            ("SSE idle timeout", self.sse_idle_timeout_seconds),
            (
                "WebSocket idle timeout",
                self.websocket_idle_timeout_seconds,
            ),
        ] {
            if let Some(seconds) = value {
                if !(0..=86400).contains(&seconds) {
                    return Err(format!(
                        "{name} {seconds} is not in valid range (0-86400 seconds, 0 = no timeout)"
                    ));
                }
            }
        }

        if let Some(max_conn) = self.max_concurrent_connections {
            if max_conn < 0 {
                return Err(format!(
                    "max_concurrent_connections cannot be negative, got {}",
                    max_conn
                ));
            }
        }

        if let Some(health) = &self.health {
            health.validate()?;
        }

        if let Some(seconds) = self.health_check_timeout_seconds {
            if !(MIN_HEALTH_CHECK_TIMEOUT_SECONDS..=MAX_HEALTH_CHECK_TIMEOUT_SECONDS)
                .contains(&seconds)
            {
                return Err(format!(
                    "Startup health-check timeout {seconds} is not in valid range \
                     ({MIN_HEALTH_CHECK_TIMEOUT_SECONDS}-{MAX_HEALTH_CHECK_TIMEOUT_SECONDS} seconds)"
                ));
            }
        }

        if let Some(seconds) = self.stop_timeout_seconds {
            validate_stop_timeout_seconds(seconds)?;
        }

        Ok(())
    }

    fn check_against_tenant_ceilings(
        &self,
        ceilings: &crate::TenantResourceCeilings,
    ) -> Result<(), Vec<String>> {
        if ceilings.is_unenforced() {
            return Ok(());
        }

        let mut violations = Vec::new();

        // `<= 0`, not `== 0`, on both of these. `validate()` rejects negatives,
        // but the ceiling must not depend on that: a negative memory limit is
        // *also* uncapped in practice — `parse_memory_mb` returns `None` for
        // anything below zero and the deployer then leaves Docker's cgroup cap
        // unset entirely. Matching only the `0` sentinel would let `-1` through
        // as "some finite value below the ceiling" and produce exactly the
        // unlimited container the ceiling exists to prevent.
        if ceilings.max_memory_limit_mb > 0 {
            let ceiling = ceilings.max_memory_limit_mb as i64;
            match self.memory_limit {
                Some(mb) if mb <= 0 => violations.push(format!(
                    "Memory limit cannot be set to unlimited: this instance caps it at {ceiling} MB"
                )),
                Some(mb) if i64::from(mb) > ceiling => violations.push(format!(
                    "Memory limit {mb} MB exceeds this instance's maximum of {ceiling} MB"
                )),
                _ => {}
            }
        }

        if ceilings.max_concurrent_connections > 0 {
            let ceiling = ceilings.max_concurrent_connections as i64;
            match self.max_concurrent_connections {
                Some(n) if n <= 0 => violations.push(format!(
                    "Concurrent connections cannot be set to unlimited: this instance caps them at {ceiling}"
                )),
                Some(n) if i64::from(n) > ceiling => violations.push(format!(
                    "Concurrent connection limit {n} exceeds this instance's maximum of {ceiling}"
                )),
                _ => {}
            }
        }

        // Nonzero timeouts need no ceiling here — they are already clamped to
        // `request_timeouts.max_*` at resolution time. Only the `Some(0)`
        // "no timeout" sentinel escapes that clamp, so that is all this checks.
        if !ceilings.allow_unlimited_request_timeouts {
            for (name, value) in [
                ("Request timeout", self.request_timeout_seconds),
                ("SSE idle timeout", self.sse_idle_timeout_seconds),
                (
                    "WebSocket idle timeout",
                    self.websocket_idle_timeout_seconds,
                ),
            ] {
                if value == Some(0) {
                    violations.push(format!(
                        "{name} cannot be set to unlimited (0) on this instance"
                    ));
                }
            }
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }

    fn effective_health_check_timeout_seconds(&self) -> u64 {
        resolve_health_check_timeout_seconds(self.health_check_timeout_seconds)
    }
}

pub trait DeploymentConfigSnapshotExt {
    /// Validate the resource configuration
    fn validate(&self) -> Result<(), String>;
}

impl DeploymentConfigSnapshotExt for DeploymentConfigSnapshot {
    fn validate(&self) -> Result<(), String> {
        // CPU request should not exceed CPU limit. A limit of 0 is the explicit
        // "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.cpu_request, self.cpu_limit) {
            if limit != 0 && request > limit {
                return Err("CPU request cannot exceed CPU limit".to_string());
            }
        }

        // Memory request should not exceed memory limit. A limit of 0 is the
        // explicit "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.memory_request, self.memory_limit) {
            if limit != 0 && request > limit {
                return Err("Memory request cannot exceed memory limit".to_string());
            }
        }

        // Port should be in valid range
        if let Some(port) = self.exposed_port {
            if !(1..=65535).contains(&port) {
                return Err(format!("Port {} is not in valid range (1-65535)", port));
            }
        }

        Ok(())
    }
}

pub trait EnvironmentDeploymentConfigExt {
    /// Get the effective deployment configuration by merging project and environment configs
    ///
    /// The project configuration serves as defaults, and the environment configuration
    /// overrides specific values. This allows setting project-wide defaults with
    /// environment-specific overrides.
    fn get_effective_deployment_config(
        &self,
        project_config: &DeploymentConfig,
    ) -> DeploymentConfig;

    /// Get the effective security configuration by merging global, project, and environment configs
    ///
    /// The inheritance chain is: Environment > Project > Global
    /// This allows:
    /// - Global defaults for all projects
    /// - Project-specific overrides (from project.deployment_config.security)
    /// - Environment-specific overrides (from environment.deployment_config.security)
    fn get_effective_security_config(
        &self,
        global_config: &SecurityConfig,
        project_deployment_config: &DeploymentConfig,
    ) -> SecurityConfig;
}

impl EnvironmentDeploymentConfigExt for environments::Model {
    fn get_effective_deployment_config(
        &self,
        project_config: &DeploymentConfig,
    ) -> DeploymentConfig {
        let env_config = self.deployment_config.clone().unwrap_or_default();
        project_config.merge(&env_config)
    }

    fn get_effective_security_config(
        &self,
        global_config: &SecurityConfig,
        project_deployment_config: &DeploymentConfig,
    ) -> SecurityConfig {
        // Extract security configs from deployment configs
        let project_security = project_deployment_config
            .security
            .as_ref()
            .cloned()
            .unwrap_or_default();

        let env_security = self
            .deployment_config
            .as_ref()
            .and_then(|dc| dc.security.clone())
            .unwrap_or_default();

        // Chain: global -> project -> environment
        global_config.merge(&project_security).merge(&env_security)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replicas_are_refused_outside_the_supported_range() {
        assert!(validate_replicas(1).is_ok());
        assert!(validate_replicas(MAX_REPLICAS).is_ok());
        for replicas in [i32::MIN, -1, 0, MAX_REPLICAS + 1, i32::MAX] {
            let message = validate_replicas(replicas).unwrap_err();
            assert!(message.contains(&replicas.to_string()), "{message}");
        }

        let too_many = DeploymentConfig {
            replicas: MAX_REPLICAS + 1,
            ..Default::default()
        };
        assert!(too_many.validate().is_err());
    }

    /// Cross-architecture builds are emulated and slow, so they must stay off
    /// until someone asks for them — and an environment must be able to turn
    /// them off again for a project that has them on.
    #[test]
    fn test_cross_architecture_builds_merge_semantics() {
        let project_on = DeploymentConfig {
            cross_architecture_builds: Some(true),
            ..Default::default()
        };
        let env_unset = DeploymentConfig::default();
        let env_off = DeploymentConfig {
            cross_architecture_builds: Some(false),
            ..Default::default()
        };

        // Off by default: nobody pays for an emulated build they didn't ask for.
        assert_eq!(env_unset.cross_architecture_builds, None);

        // An environment that says nothing inherits the project.
        assert_eq!(
            project_on.merge(&env_unset).cross_architecture_builds,
            Some(true)
        );

        // ...and one that says `false` overrides it. An `||` merge (which is
        // what the neighbouring booleans use) would make this impossible.
        assert_eq!(
            project_on.merge(&env_off).cross_architecture_builds,
            Some(false)
        );

        // Neither set stays unset; the read site treats that as disabled.
        assert_eq!(
            DeploymentConfig::default()
                .merge(&env_unset)
                .cross_architecture_builds,
            None
        );
    }

    /// Building on the control plane stays the default; a node build is an
    /// opt-in that an environment can make or refuse independently of its
    /// project.
    #[test]
    fn test_build_location_resolution() {
        let project_node = DeploymentConfig {
            build_location: Some(BuildLocation::Node),
            ..Default::default()
        };
        let env_control_plane = DeploymentConfig {
            build_location: Some(BuildLocation::ControlPlane),
            ..Default::default()
        };
        let env_unset = DeploymentConfig::default();

        assert_eq!(
            DeploymentConfig::resolve_build_location(None, None),
            BuildLocation::ControlPlane
        );
        assert_eq!(
            DeploymentConfig::resolve_build_location(Some(&env_unset), Some(&env_unset)),
            BuildLocation::ControlPlane
        );
        assert_eq!(
            DeploymentConfig::resolve_build_location(Some(&env_unset), Some(&project_node)),
            BuildLocation::Node
        );
        assert_eq!(
            DeploymentConfig::resolve_build_location(Some(&env_control_plane), Some(&project_node)),
            BuildLocation::ControlPlane
        );
        assert_eq!(
            DeploymentConfig::resolve_build_location(Some(&project_node), None),
            BuildLocation::Node
        );
        assert_eq!(
            project_node.merge(&env_control_plane).build_location,
            Some(BuildLocation::ControlPlane)
        );
        assert_eq!(
            project_node.merge(&env_unset).build_location,
            Some(BuildLocation::Node)
        );
    }

    #[test]
    fn uncapped_limit_sentinel_does_not_constrain_request() {
        // memory_limit/cpu_limit of 0 is the explicit "uncapped" sentinel and
        // must not trip the request-exceeds-limit validation.
        let config = DeploymentConfig {
            cpu_request: Some(500_000),
            cpu_limit: Some(0),
            memory_request: Some(128),
            memory_limit: Some(0),
            ..DeploymentConfig::default()
        };
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn positive_memory_limit_below_request_is_rejected() {
        let config = DeploymentConfig {
            memory_request: Some(256),
            memory_limit: Some(128),
            ..DeploymentConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn cpu_limit_below_docker_minimum_is_rejected() {
        // 1000 microcores is 0.001 cores: Docker refuses to start the
        // container, so the config must be rejected when it is saved.
        let too_small = DeploymentConfig {
            cpu_limit: Some(1000),
            ..DeploymentConfig::default()
        };
        let err = too_small.validate().unwrap_err();
        assert!(err.contains("at least 10000 microcores"), "{err}");

        for limit in [0, MIN_CPU_LIMIT_MICROCORES, 1_000_000] {
            let config = DeploymentConfig {
                cpu_limit: Some(limit),
                ..DeploymentConfig::default()
            };
            assert_eq!(config.validate(), Ok(()), "cpu_limit {limit}");
        }
    }

    #[test]
    fn merge_keeps_env_uncapped_over_project_cap() {
        // An env that explicitly opts into uncapped (Some(0)) must win over a
        // project-level hard cap rather than inheriting it.
        let project = DeploymentConfig {
            memory_limit: Some(512),
            ..DeploymentConfig::default()
        };
        let env = DeploymentConfig {
            memory_limit: Some(0),
            ..DeploymentConfig::default()
        };
        assert_eq!(project.merge(&env).memory_limit, Some(0));
    }

    #[test]
    fn test_merge_configs() {
        let project_config = DeploymentConfig {
            cpu_request: Some(100),
            cpu_limit: Some(1000),
            memory_request: Some(128),
            memory_limit: Some(512),
            exposed_port: Some(3000),
            automatic_deploy: Some(true),
            performance_metrics_enabled: true,
            session_recording_enabled: false,
            replicas: 2,
            security: None,
            target_nodes: None,
            target_labels: None,
            anti_affinity: true,
            ..Default::default()
        };

        let env_config = DeploymentConfig {
            cpu_request: Some(200),   // Override
            cpu_limit: None,          // Use project default
            memory_request: None,     // Use project default
            memory_limit: Some(1024), // Override
            exposed_port: Some(8080), // Override
            // Env explicitly opts out — env-wins semantics means this is respected
            automatic_deploy: Some(false),
            performance_metrics_enabled: false,
            session_recording_enabled: true, // Override
            replicas: 5,                     // Override
            security: None,
            target_nodes: None,
            target_labels: None,
            anti_affinity: true,
            ..Default::default()
        };

        let merged = project_config.merge(&env_config);

        assert_eq!(merged.cpu_request, Some(200));
        assert_eq!(merged.cpu_limit, Some(1000));
        assert_eq!(merged.memory_request, Some(128));
        assert_eq!(merged.memory_limit, Some(1024));
        assert_eq!(merged.exposed_port, Some(8080));
        // Env explicitly sets false → env wins, merged is false
        assert_eq!(merged.automatic_deploy, Some(false));
        assert!(merged.performance_metrics_enabled); // true || false = true
        assert!(merged.session_recording_enabled);
        assert_eq!(merged.replicas, 5);
        assert_eq!(merged.target_nodes, None);
    }

    #[test]
    fn test_merge_target_nodes() {
        // Environment target_nodes overrides project-level
        let project_config = DeploymentConfig {
            target_nodes: Some(vec![1, 2]),
            ..Default::default()
        };
        let env_config = DeploymentConfig {
            target_nodes: Some(vec![3]),
            ..Default::default()
        };
        let merged = project_config.merge(&env_config);
        assert_eq!(merged.target_nodes, Some(vec![3]));

        // Falls back to project-level when env has None
        let env_no_nodes = DeploymentConfig {
            target_nodes: None,
            ..Default::default()
        };
        let merged2 = project_config.merge(&env_no_nodes);
        assert_eq!(merged2.target_nodes, Some(vec![1, 2]));

        // A legacy persisted empty environment selector means the override was
        // cleared; it must inherit rather than erase the project constraint.
        let env_empty_nodes = DeploymentConfig {
            target_nodes: Some(vec![]),
            ..Default::default()
        };
        let merged3 = project_config.merge(&env_empty_nodes);
        assert_eq!(merged3.target_nodes, Some(vec![1, 2]));

        // Both None → None
        let both_none = DeploymentConfig::default().merge(&DeploymentConfig::default());
        assert_eq!(both_none.target_nodes, None);
    }

    #[test]
    fn test_empty_environment_target_labels_inherit_project_constraint() {
        let project_config = DeploymentConfig {
            target_labels: Some(serde_json::json!({"region": "eu"})),
            ..Default::default()
        };
        let env_config = DeploymentConfig {
            target_labels: Some(serde_json::json!({})),
            ..Default::default()
        };

        let merged = project_config.merge(&env_config);

        assert_eq!(
            merged.target_labels,
            Some(serde_json::json!({"region": "eu"}))
        );
    }

    #[test]
    fn stop_timeout_defaults_validates_merges_and_clamps() {
        // Unset keeps today's 10 s grace, so existing projects are unchanged.
        assert_eq!(resolve_stop_timeout_seconds(None), 10);
        assert_eq!(resolve_stop_timeout_seconds(Some(120)), 120);
        // A hand-edited row can neither remove the grace nor hold for days.
        assert_eq!(resolve_stop_timeout_seconds(Some(0)), 1);
        assert_eq!(resolve_stop_timeout_seconds(Some(-5)), 1);
        assert_eq!(resolve_stop_timeout_seconds(Some(i32::MAX)), 3600);

        for ok in [1, 10, 3600] {
            let config = DeploymentConfig {
                stop_timeout_seconds: Some(ok),
                ..Default::default()
            };
            assert!(config.validate().is_ok(), "{ok} is in range");
        }
        for bad in [0, -1, 3601] {
            let config = DeploymentConfig {
                stop_timeout_seconds: Some(bad),
                ..Default::default()
            };
            let error = config.validate().expect_err("out of range");
            assert!(error.contains(&format!("Stop timeout {bad}")), "{error}");
            assert!(error.contains("1-3600 seconds"), "{error}");
        }

        let project = DeploymentConfig {
            stop_timeout_seconds: Some(120),
            ..Default::default()
        };
        assert_eq!(
            project
                .merge(&DeploymentConfig::default())
                .stop_timeout_seconds,
            Some(120),
            "an environment without an override inherits the project value"
        );
        let env = DeploymentConfig {
            stop_timeout_seconds: Some(30),
            ..Default::default()
        };
        assert_eq!(project.merge(&env).stop_timeout_seconds, Some(30));

        // Stored configs written before the field existed still parse.
        let legacy: DeploymentConfig =
            serde_json::from_value(serde_json::json!({"replicas": 1})).expect("legacy config");
        assert_eq!(legacy.stop_timeout_seconds, None);
    }

    #[test]
    fn health_check_timeout_defaults_validates_and_merges() {
        let project = DeploymentConfig::default();
        assert_eq!(project.effective_health_check_timeout_seconds(), 300);

        let project = DeploymentConfig {
            health_check_timeout_seconds: Some(900),
            ..Default::default()
        };
        assert!(project.validate().is_ok());
        assert_eq!(project.effective_health_check_timeout_seconds(), 900);

        // Environment inherits the project value, and may override it.
        let env = DeploymentConfig::default();
        assert_eq!(project.merge(&env).health_check_timeout_seconds, Some(900));
        let env = DeploymentConfig {
            health_check_timeout_seconds: Some(60),
            ..Default::default()
        };
        assert_eq!(project.merge(&env).health_check_timeout_seconds, Some(60));

        for bad in [0, 29, 3601, -5] {
            let config = DeploymentConfig {
                health_check_timeout_seconds: Some(bad),
                ..Default::default()
            };
            let err = config.validate().unwrap_err();
            assert!(err.contains("30-3600"), "{err}");
            // Even an unvalidated stored value is clamped when applied.
            let applied = config.effective_health_check_timeout_seconds();
            assert!((30..=3600).contains(&applied), "{applied}");
        }

        // Old rows without the key deserialize to "inherit".
        let legacy: DeploymentConfig = serde_json::from_str(r#"{"replicas":1}"#).unwrap();
        assert_eq!(legacy.health_check_timeout_seconds, None);
    }

    #[test]
    fn merge_env_request_timeout_overrides_project() {
        // Three-state semantics (like memory_limit, not the idle_timeout_seconds
        // sentinel pattern): env None must inherit project's value, env Some
        // must win outright.
        let project = DeploymentConfig {
            request_timeout_seconds: Some(30),
            sse_idle_timeout_seconds: Some(1800),
            websocket_idle_timeout_seconds: Some(1800),
            ..Default::default()
        };

        let env_override = DeploymentConfig {
            request_timeout_seconds: Some(10),
            ..Default::default()
        };
        let merged = project.merge(&env_override);
        assert_eq!(merged.request_timeout_seconds, Some(10));
        // Env left these unset -> inherit project's values.
        assert_eq!(merged.sse_idle_timeout_seconds, Some(1800));
        assert_eq!(merged.websocket_idle_timeout_seconds, Some(1800));

        let env_no_override = DeploymentConfig::default();
        let merged2 = project.merge(&env_no_override);
        assert_eq!(merged2.request_timeout_seconds, Some(30));
    }

    #[test]
    fn request_timeout_validation_rejects_out_of_range() {
        // 0 is a valid explicit "no timeout" override, not a validation error.
        let no_timeout_override = DeploymentConfig {
            request_timeout_seconds: Some(0),
            ..Default::default()
        };
        assert!(no_timeout_override.validate().is_ok());

        let negative = DeploymentConfig {
            request_timeout_seconds: Some(-1),
            ..Default::default()
        };
        assert!(negative.validate().is_err());

        let too_high = DeploymentConfig {
            sse_idle_timeout_seconds: Some(90_000),
            ..Default::default()
        };
        assert!(too_high.validate().is_err());

        let ok = DeploymentConfig {
            websocket_idle_timeout_seconds: Some(3600),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn test_validation() {
        let valid_config = DeploymentConfig {
            cpu_request: Some(100_000),
            cpu_limit: Some(1_000_000),
            memory_request: Some(128),
            memory_limit: Some(512),
            exposed_port: Some(3000),
            security: None,
            ..Default::default()
        };
        assert!(valid_config.validate().is_ok());

        let invalid_cpu = DeploymentConfig {
            cpu_request: Some(2_000_000),
            cpu_limit: Some(1_000_000),
            security: None,
            ..Default::default()
        };
        assert!(invalid_cpu.validate().is_err());

        let invalid_memory = DeploymentConfig {
            memory_request: Some(1024),
            memory_limit: Some(512),
            security: None,
            ..Default::default()
        };
        assert!(invalid_memory.validate().is_err());

        let invalid_port = DeploymentConfig {
            exposed_port: Some(70000),
            ..Default::default()
        };
        assert!(invalid_port.validate().is_err());
    }

    /// The whole contract of the default ceilings: an instance that has never
    /// configured them accepts exactly what it accepted before, including the
    /// `0 = unlimited` sentinels.
    #[test]
    fn default_ceilings_accept_every_unlimited_sentinel() {
        let ceilings = crate::TenantResourceCeilings::default();
        let config = DeploymentConfig {
            memory_limit: Some(0),
            max_concurrent_connections: Some(0),
            request_timeout_seconds: Some(0),
            sse_idle_timeout_seconds: Some(0),
            websocket_idle_timeout_seconds: Some(0),
            ..Default::default()
        };

        assert!(config.check_against_tenant_ceilings(&ceilings).is_ok());
    }

    #[test]
    fn configured_ceilings_reject_unlimited_and_over_ceiling_values() {
        let ceilings = crate::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            max_concurrent_connections: 200,
            allow_unlimited_request_timeouts: false,
        };

        // `0` means "opt out of the global cap" — the exact thing a ceiling exists to stop.
        let unlimited = DeploymentConfig {
            memory_limit: Some(0),
            max_concurrent_connections: Some(0),
            request_timeout_seconds: Some(0),
            sse_idle_timeout_seconds: Some(0),
            websocket_idle_timeout_seconds: Some(0),
            ..Default::default()
        };
        let violations = unlimited
            .check_against_tenant_ceilings(&ceilings)
            .expect_err("unlimited overrides must be rejected under ceilings");
        // All five reported at once, not just the first.
        assert_eq!(violations.len(), 5, "got: {violations:?}");

        let over = DeploymentConfig {
            memory_limit: Some(8192),
            max_concurrent_connections: Some(500),
            ..Default::default()
        };
        let violations = over
            .check_against_tenant_ceilings(&ceilings)
            .expect_err("values above the ceiling must be rejected");
        assert_eq!(violations.len(), 2, "got: {violations:?}");
        // The message has to name the ceiling, or the user cannot pick a legal value.
        assert!(violations[0].contains("4096"), "got: {violations:?}");
        assert!(violations[1].contains("200"), "got: {violations:?}");
    }

    #[test]
    fn configured_ceilings_accept_values_at_and_below_the_ceiling() {
        let ceilings = crate::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            max_concurrent_connections: 200,
            allow_unlimited_request_timeouts: false,
        };

        let at_ceiling = DeploymentConfig {
            memory_limit: Some(4096),
            max_concurrent_connections: Some(200),
            // Nonzero timeouts are clamped at resolution time, not here.
            request_timeout_seconds: Some(3600),
            ..Default::default()
        };
        assert!(at_ceiling.check_against_tenant_ceilings(&ceilings).is_ok());

        // `None` = inherit the instance default, which is always within the ceiling.
        let inheriting = DeploymentConfig::default();
        assert!(inheriting.check_against_tenant_ceilings(&ceilings).is_ok());
    }

    /// Regression: a negative limit is uncapped in practice, so it must not be
    /// able to slip past a ceiling by looking like "a small finite value".
    ///
    /// `-1` formats to `"-1Mi"`, `parse_memory_mb` returns `None` for anything
    /// below zero, and the deployer leaves Docker's memory cgroup unset — the
    /// exact outcome `Some(0)` is refused for.
    #[test]
    fn negative_limits_are_treated_as_unlimited_by_the_ceiling() {
        let ceilings = crate::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            max_concurrent_connections: 200,
            ..crate::TenantResourceCeilings::default()
        };
        let negative = DeploymentConfig {
            memory_limit: Some(-1),
            max_concurrent_connections: Some(-1),
            ..Default::default()
        };

        let violations = negative
            .check_against_tenant_ceilings(&ceilings)
            .expect_err("a negative limit is uncapped and must be refused");
        assert_eq!(violations.len(), 2, "got: {violations:?}");
        assert!(violations[0].contains("unlimited"), "got: {violations:?}");
    }

    /// Defence in depth for the same hole: a negative never reaches the ceiling
    /// check because it is not a storable value in the first place.
    #[test]
    fn validation_rejects_negative_resource_values() {
        for config in [
            DeploymentConfig {
                memory_limit: Some(-1),
                ..Default::default()
            },
            DeploymentConfig {
                memory_request: Some(-1),
                ..Default::default()
            },
            DeploymentConfig {
                cpu_limit: Some(-1),
                ..Default::default()
            },
            DeploymentConfig {
                cpu_request: Some(-1),
                ..Default::default()
            },
        ] {
            let error = config
                .validate()
                .expect_err("negative resource values must be rejected");
            assert!(error.contains("cannot be negative"), "got: {error}");
        }

        // `0` stays valid — it is the documented "unlimited" sentinel.
        let uncapped = DeploymentConfig {
            memory_limit: Some(0),
            cpu_limit: Some(0),
            ..Default::default()
        };
        assert!(uncapped.validate().is_ok());
    }

    /// Ceilings are independent: setting one must not start enforcing the others.
    #[test]
    fn ceilings_are_enforced_independently() {
        let memory_only = crate::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            ..crate::TenantResourceCeilings::default()
        };
        let config = DeploymentConfig {
            memory_limit: Some(1024),
            max_concurrent_connections: Some(0),
            request_timeout_seconds: Some(0),
            ..Default::default()
        };

        assert!(config.check_against_tenant_ceilings(&memory_only).is_ok());
    }

    #[test]
    fn snapshot_validation_rejects_inverted_resources_and_bad_ports() {
        assert!(DeploymentConfigSnapshot::default().validate().is_ok());
        let cpu = DeploymentConfigSnapshot {
            cpu_request: Some(200),
            cpu_limit: Some(100),
            ..Default::default()
        };
        assert_eq!(
            cpu.validate().unwrap_err(),
            "CPU request cannot exceed CPU limit"
        );
        let uncapped = DeploymentConfigSnapshot {
            memory_request: Some(200),
            memory_limit: Some(0),
            ..Default::default()
        };
        assert!(uncapped.validate().is_ok());
        let memory = DeploymentConfigSnapshot {
            memory_request: Some(200),
            memory_limit: Some(100),
            ..Default::default()
        };
        assert_eq!(
            memory.validate().unwrap_err(),
            "Memory request cannot exceed memory limit"
        );
        let port = DeploymentConfigSnapshot {
            exposed_port: Some(70_000),
            ..Default::default()
        };
        assert!(port.validate().unwrap_err().contains("70000"));
    }

    #[test]
    fn resolved_health_check_timeout_is_clamped_and_defaulted() {
        assert_eq!(
            resolve_health_check_timeout_seconds(None),
            DEFAULT_HEALTH_CHECK_TIMEOUT_SECONDS as u64
        );
        assert_eq!(
            resolve_health_check_timeout_seconds(Some(0)),
            MIN_HEALTH_CHECK_TIMEOUT_SECONDS as u64
        );
        assert_eq!(
            resolve_health_check_timeout_seconds(Some(i32::MAX)),
            MAX_HEALTH_CHECK_TIMEOUT_SECONDS as u64
        );
    }

    #[test]
    fn empty_selectors_are_not_configured() {
        let empty = DeploymentConfig {
            target_nodes: Some(vec![]),
            target_labels: Some(serde_json::json!({})),
            ..Default::default()
        };
        assert_eq!(empty.configured_target_nodes(), None);
        assert_eq!(empty.configured_target_labels(), None);
        let set = DeploymentConfig {
            target_nodes: Some(vec![7]),
            target_labels: Some(serde_json::json!({"disk": "ssd"})),
            ..Default::default()
        };
        assert_eq!(set.configured_target_nodes(), Some(&[7][..]));
        assert_eq!(
            set.configured_target_labels(),
            Some(&serde_json::json!({"disk": "ssd"}))
        );
    }

    #[test]
    fn security_chain_prefers_environment_then_project_then_global() {
        let global = SecurityConfig {
            attack_mode: Some("off".into()),
            enabled: Some(true),
            ..Default::default()
        };
        let project = DeploymentConfig {
            security: Some(SecurityConfig {
                attack_mode: Some("on".into()),
                rate_limiting: Some(RateLimitConfig {
                    max_requests_per_minute: Some(10),
                    max_requests_per_hour: None,
                    whitelist_ips: vec!["10.0.0.2".into(), "10.0.0.1".into()],
                    blacklist_ips: vec![],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let env = environment_model(Some(DeploymentConfig {
            security: Some(SecurityConfig {
                rate_limiting: Some(RateLimitConfig {
                    max_requests_per_minute: Some(5),
                    max_requests_per_hour: None,
                    whitelist_ips: vec!["10.0.0.1".into()],
                    blacklist_ips: vec![],
                }),
                ..Default::default()
            }),
            ..Default::default()
        }));

        let effective = env.get_effective_security_config(&global, &project);

        assert_eq!(effective.enabled, Some(true));
        assert_eq!(effective.attack_mode.as_deref(), Some("on"));
        let rate_limit = effective.rate_limiting.expect("rate limit is inherited");
        assert_eq!(rate_limit.max_requests_per_minute, Some(5));
        assert_eq!(
            rate_limit.whitelist_ips,
            vec!["10.0.0.1".to_string(), "10.0.0.2".to_string()]
        );
    }

    #[test]
    fn security_merge_keeps_either_side_and_prefers_other_headers() {
        let base = SecurityConfig {
            headers: Some(SecurityHeadersConfig {
                preset: Some("strict".into()),
                content_security_policy: None,
                x_frame_options: Some("DENY".into()),
                strict_transport_security: None,
                referrer_policy: None,
            }),
            ..Default::default()
        };
        let other = SecurityConfig {
            headers: Some(SecurityHeadersConfig {
                preset: None,
                content_security_policy: None,
                x_frame_options: Some("SAMEORIGIN".into()),
                strict_transport_security: None,
                referrer_policy: None,
            }),
            ..Default::default()
        };
        let merged = base.merge(&other).headers.expect("headers are merged");
        assert_eq!(merged.preset.as_deref(), Some("strict"));
        assert_eq!(merged.x_frame_options.as_deref(), Some("SAMEORIGIN"));
        assert_eq!(base.merge(&SecurityConfig::default()).headers, base.headers);
        assert_eq!(SecurityConfig::default().merge(&base).headers, base.headers);
    }

    #[test]
    fn effective_deployment_config_lets_the_environment_override_the_project() {
        let project = DeploymentConfig {
            memory_limit: Some(512),
            cpu_limit: Some(100_000),
            ..Default::default()
        };
        let env = environment_model(Some(DeploymentConfig {
            memory_limit: Some(1024),
            ..Default::default()
        }));
        let effective = env.get_effective_deployment_config(&project);
        assert_eq!(effective.memory_limit, Some(1024));
        assert_eq!(effective.cpu_limit, Some(100_000));

        let unset = environment_model(None);
        assert_eq!(
            unset.get_effective_deployment_config(&project).memory_limit,
            Some(512)
        );
    }

    fn environment_model(deployment_config: Option<DeploymentConfig>) -> environments::Model {
        let now = chrono::Utc::now();
        environments::Model {
            id: 1,
            name: "production".into(),
            slug: "production".into(),
            subdomain: "production".into(),
            last_deployment: None,
            host: String::new(),
            upstreams: Default::default(),
            created_at: now,
            updated_at: now,
            project_id: 1,
            current_deployment_id: None,
            branch: None,
            deleted_at: None,
            deployment_config,
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            shield_mode: None,
            shield_exempt_paths: Default::default(),
            force_https: None,
            last_activity_at: None,
            ingress_mode: Default::default(),
            workload_mode: Default::default(),
        }
    }
}

#[cfg(test)]
mod health_probe_tests {
    use super::*;
    use temps_core::repo_config::HealthCheckConfig;

    #[test]
    fn health_probe_settings_inherit_and_validate() {
        let project = DeploymentConfig {
            health: Some(HealthCheckConfig {
                probe_type: Some("process".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            project.merge(&DeploymentConfig::default()).health,
            project.health
        );
        let environment = DeploymentConfig {
            health: Some(HealthCheckConfig {
                probe_type: Some("exec".into()),
                command: vec!["redis-cli".into(), "ping".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(project.merge(&environment).health, environment.health);
        assert!(environment.validate().is_ok());
        let bad = DeploymentConfig {
            health: Some(HealthCheckConfig {
                probe_type: Some("unsupported".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }
}
