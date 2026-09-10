// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Immutable process runtime configuration and service endpoint resolution.
//!
//! The execution environment is parsed once at the CLI composition root. Domain
//! services receive the resulting context (or its environment value) rather
//! than consulting process-global environment variables while handling work.

use std::fmt;
use std::sync::OnceLock;

use thiserror::Error;

/// Canonical bootstrap setting describing where the Temps control plane runs.
pub const EXECUTION_ENVIRONMENT_VARIABLE: &str = "TEMPS_EXECUTION_ENV";

/// Deprecated bootstrap setting retained for existing installations.
pub const LEGACY_DEPLOYMENT_MODE_VARIABLE: &str = "DEPLOYMENT_MODE";

/// Where the Temps control-plane process is executing.
///
/// This describes network location only. It does not assert that Docker, KVM,
/// host networking, or another workload capability is available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExecutionEnvironment {
    /// Native process managed directly by the host operating system.
    #[default]
    Host,
    /// Process running inside a Docker container network.
    Docker,
}

impl fmt::Display for ExecutionEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => formatter.write_str("host"),
            Self::Docker => formatter.write_str("docker"),
        }
    }
}

/// Which bootstrap input selected the execution environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionEnvironmentSource {
    /// `TEMPS_EXECUTION_ENV` was explicitly configured.
    Canonical,
    /// Deprecated `DEPLOYMENT_MODE` was used because the canonical input was absent.
    Legacy,
    /// Neither input was set; the backward-compatible host default was selected.
    Default,
}

/// A network scheme carried separately from a resolved host and port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceEndpointScheme {
    Http,
    Https,
    Postgres,
    Redis,
    Tcp,
}

impl fmt::Display for ServiceEndpointScheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http => formatter.write_str("http"),
            Self::Https => formatter.write_str("https"),
            Self::Postgres => formatter.write_str("postgres"),
            Self::Redis => formatter.write_str("redis"),
            Self::Tcp => formatter.write_str("tcp"),
        }
    }
}

/// A resolved endpoint with its network metadata kept as typed fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndpoint {
    pub scheme: ServiceEndpointScheme,
    pub host: String,
    pub port: u16,
    pub tls_server_name: Option<String>,
}

impl ServiceEndpoint {
    /// Render an absolute URL for URL-oriented schemes.
    pub fn url(&self, path: Option<&str>) -> String {
        format!(
            "{}://{}:{}{}",
            self.scheme,
            self.host,
            self.port,
            path.unwrap_or_default()
        )
    }

    /// Render the host/port authority used by proxy upstream configuration.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Pure environment-specific endpoint resolver selected at process startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEndpointResolver {
    Host,
    Docker,
}

impl ServiceEndpointResolver {
    /// Resolve a stable service identity without rewriting an existing URL.
    ///
    /// Host execution uses IPv4 loopback and the published host port. Docker
    /// execution uses the stable service/container name and its internal target
    /// port, independently of any host publication.
    pub fn resolve(
        self,
        service_name: &str,
        scheme: ServiceEndpointScheme,
        internal_target_port: u16,
        published_host_port: u16,
    ) -> ServiceEndpoint {
        match self {
            Self::Host => ServiceEndpoint {
                scheme,
                host: "127.0.0.1".to_string(),
                port: published_host_port,
                tls_server_name: None,
            },
            Self::Docker => ServiceEndpoint {
                scheme,
                host: service_name.to_string(),
                port: internal_target_port,
                tls_server_name: None,
            },
        }
    }
}

/// Immutable runtime configuration shared by infrastructure-facing services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContext {
    execution_environment: ExecutionEnvironment,
    endpoint_resolver: ServiceEndpointResolver,
    source: ExecutionEnvironmentSource,
}

impl RuntimeContext {
    pub fn host() -> Self {
        Self::new(
            ExecutionEnvironment::Host,
            ExecutionEnvironmentSource::Default,
        )
    }

    pub fn docker() -> Self {
        Self::new(
            ExecutionEnvironment::Docker,
            ExecutionEnvironmentSource::Canonical,
        )
    }

    pub fn for_environment(execution_environment: ExecutionEnvironment) -> Self {
        Self::new(execution_environment, ExecutionEnvironmentSource::Canonical)
    }

    fn new(
        execution_environment: ExecutionEnvironment,
        source: ExecutionEnvironmentSource,
    ) -> Self {
        let endpoint_resolver = match execution_environment {
            ExecutionEnvironment::Host => ServiceEndpointResolver::Host,
            ExecutionEnvironment::Docker => ServiceEndpointResolver::Docker,
        };
        Self {
            execution_environment,
            endpoint_resolver,
            source,
        }
    }

    /// Parse canonical and legacy values with deterministic precedence.
    pub fn from_configured_values(
        canonical: Option<&str>,
        legacy: Option<&str>,
    ) -> Result<Self, RuntimeConfigurationError> {
        let (variable, configured, source) = if let Some(value) = canonical {
            (
                EXECUTION_ENVIRONMENT_VARIABLE,
                Some(value),
                ExecutionEnvironmentSource::Canonical,
            )
        } else if let Some(value) = legacy {
            (
                LEGACY_DEPLOYMENT_MODE_VARIABLE,
                Some(value),
                ExecutionEnvironmentSource::Legacy,
            )
        } else {
            (
                EXECUTION_ENVIRONMENT_VARIABLE,
                None,
                ExecutionEnvironmentSource::Default,
            )
        };

        let execution_environment = match configured.map(str::trim) {
            None => ExecutionEnvironment::Host,
            Some(value) if value.eq_ignore_ascii_case("host") => ExecutionEnvironment::Host,
            Some(value) if value.eq_ignore_ascii_case("baremetal") => ExecutionEnvironment::Host,
            Some(value) if value.eq_ignore_ascii_case("docker") => ExecutionEnvironment::Docker,
            Some(value) if value.eq_ignore_ascii_case("kubernetes") => {
                return Err(RuntimeConfigurationError::EnvironmentNotSupported {
                    variable,
                    configured_value: value.to_string(),
                    reason: "Kubernetes execution is not supported in this build; use 'host' or 'docker'",
                });
            }
            Some(value) => {
                return Err(RuntimeConfigurationError::InvalidExecutionEnvironment {
                    variable,
                    configured_value: value.to_string(),
                    accepted_values: "host, baremetal (legacy), docker",
                });
            }
        };

        Ok(Self::new(execution_environment, source))
    }

    /// Read bootstrap configuration from the process environment.
    ///
    /// Application startup should call [`initialize_process_runtime_context`]
    /// instead so the read is cached for the lifetime of the process.
    fn from_process_environment() -> Result<Self, RuntimeConfigurationError> {
        if let Some(canonical) = read_environment_variable(EXECUTION_ENVIRONMENT_VARIABLE)? {
            return Self::from_configured_values(Some(&canonical), None);
        }

        let legacy = read_environment_variable(LEGACY_DEPLOYMENT_MODE_VARIABLE)?;
        Self::from_configured_values(None, legacy.as_deref())
    }

    pub fn execution_environment(&self) -> ExecutionEnvironment {
        self.execution_environment
    }

    pub fn source(&self) -> ExecutionEnvironmentSource {
        self.source
    }

    pub fn endpoint_resolver(&self) -> ServiceEndpointResolver {
        self.endpoint_resolver
    }

    pub fn resolve_service_endpoint(
        &self,
        service_name: &str,
        scheme: ServiceEndpointScheme,
        internal_target_port: u16,
        published_host_port: u16,
    ) -> ServiceEndpoint {
        self.endpoint_resolver.resolve(
            service_name,
            scheme,
            internal_target_port,
            published_host_port,
        )
    }
}

/// Typed startup failures for invalid execution-environment configuration.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RuntimeConfigurationError {
    #[error(
        "Invalid execution environment '{configured_value}' from {variable}; expected one of: {accepted_values}"
    )]
    InvalidExecutionEnvironment {
        variable: &'static str,
        configured_value: String,
        accepted_values: &'static str,
    },

    #[error(
        "Execution environment '{configured_value}' from {variable} is not supported: {reason}"
    )]
    EnvironmentNotSupported {
        variable: &'static str,
        configured_value: String,
        reason: &'static str,
    },

    #[error(
        "Execution environment from {variable} is not valid Unicode (configured bytes: {configured_value})"
    )]
    NonUnicodeExecutionEnvironment {
        variable: &'static str,
        configured_value: String,
    },
}

fn read_environment_variable(
    variable: &'static str,
) -> Result<Option<String>, RuntimeConfigurationError> {
    match std::env::var(variable) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(value)) => {
            Err(RuntimeConfigurationError::NonUnicodeExecutionEnvironment {
                variable,
                configured_value: format!("{value:?}"),
            })
        }
    }
}

static PROCESS_RUNTIME_CONTEXT: OnceLock<Result<RuntimeContext, RuntimeConfigurationError>> =
    OnceLock::new();

/// Parse and cache the process runtime context exactly once.
pub fn initialize_process_runtime_context(
) -> Result<&'static RuntimeContext, RuntimeConfigurationError> {
    PROCESS_RUNTIME_CONTEXT
        .get_or_init(RuntimeContext::from_process_environment)
        .as_ref()
        .map_err(Clone::clone)
}

/// Return the already-initialized execution environment to deprecated callers.
///
/// The compatibility path intentionally performs no environment read. CLI
/// startup initializes the context before constructing services. Library-only
/// legacy callers that skip composition-root initialization retain historical
/// Host behavior until they migrate to an injected [`RuntimeContext`].
#[doc(hidden)]
pub fn execution_environment_compatibility() -> ExecutionEnvironment {
    PROCESS_RUNTIME_CONTEXT
        .get()
        .and_then(|result| result.as_ref().ok())
        .map(RuntimeContext::execution_environment)
        .unwrap_or(ExecutionEnvironment::Host)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROCESS_ENVIRONMENT_CHILD: &str = "TEMPS_RUNTIME_PROCESS_ENVIRONMENT_CHILD";
    const NON_UNICODE_CHILD: &str = "TEMPS_RUNTIME_NON_UNICODE_CHILD";

    fn run_exact_test_in_child(
        test_name: &str,
        marker: &str,
        canonical_value: &std::ffi::OsStr,
    ) -> std::process::ExitStatus {
        std::process::Command::new(std::env::current_exe().expect("test executable should exist"))
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(marker, "1")
            .env(EXECUTION_ENVIRONMENT_VARIABLE, canonical_value)
            .env_remove(LEGACY_DEPLOYMENT_MODE_VARIABLE)
            .status()
            .expect("runtime environment child test should start")
    }

    #[test]
    fn canonical_value_has_precedence_over_legacy_value() {
        let context = RuntimeContext::from_configured_values(Some("host"), Some("docker"))
            .expect("known environment should parse");
        assert_eq!(context.execution_environment(), ExecutionEnvironment::Host);
        assert_eq!(context.source(), ExecutionEnvironmentSource::Canonical);
    }

    #[test]
    fn test_from_configured_values_invalid_canonical_over_valid_legacy_returns_canonical_error() {
        let error = RuntimeContext::from_configured_values(Some("swarm"), Some("docker"))
            .expect_err("an invalid canonical value must not fall back to legacy configuration");

        assert!(matches!(
            error,
            RuntimeConfigurationError::InvalidExecutionEnvironment {
                variable: EXECUTION_ENVIRONMENT_VARIABLE,
                configured_value,
                ..
            } if configured_value == "swarm"
        ));
    }

    #[test]
    fn legacy_and_unset_values_preserve_host_compatibility() {
        let legacy = RuntimeContext::from_configured_values(None, Some("baremetal"))
            .expect("legacy baremetal should parse");
        let unset = RuntimeContext::from_configured_values(None, None)
            .expect("unset environment should default to host");
        assert_eq!(legacy.execution_environment(), ExecutionEnvironment::Host);
        assert_eq!(legacy.source(), ExecutionEnvironmentSource::Legacy);
        assert_eq!(unset.execution_environment(), ExecutionEnvironment::Host);
        assert_eq!(unset.source(), ExecutionEnvironmentSource::Default);
    }

    #[test]
    fn configured_values_are_case_insensitive() {
        let host = RuntimeContext::from_configured_values(Some("HOST"), None)
            .expect("uppercase host should parse");
        let docker = RuntimeContext::from_configured_values(None, Some("DoCkEr"))
            .expect("mixed-case legacy docker should parse");
        assert_eq!(host.execution_environment(), ExecutionEnvironment::Host);
        assert_eq!(docker.execution_environment(), ExecutionEnvironment::Docker);
    }

    #[test]
    fn kubernetes_is_recognized_but_rejected_by_this_build() {
        let error = RuntimeContext::from_configured_values(Some("kubernetes"), None)
            .expect_err("Kubernetes must fail until its runtime adapter exists");
        assert!(matches!(
            error,
            RuntimeConfigurationError::EnvironmentNotSupported {
                variable: EXECUTION_ENVIRONMENT_VARIABLE,
                configured_value,
                ..
            } if configured_value == "kubernetes"
        ));
    }

    #[test]
    fn unknown_values_fail_with_the_configured_source() {
        let error = RuntimeContext::from_configured_values(None, Some("swarm"))
            .expect_err("unknown environment must fail closed");
        assert!(matches!(
            error,
            RuntimeConfigurationError::InvalidExecutionEnvironment {
                variable: LEGACY_DEPLOYMENT_MODE_VARIABLE,
                configured_value,
                ..
            } if configured_value == "swarm"
        ));
    }

    #[test]
    fn endpoint_resolution_uses_environment_specific_ports() {
        let host = RuntimeContext::host().resolve_service_endpoint(
            "temps-clickhouse",
            ServiceEndpointScheme::Http,
            8123,
            18123,
        );
        let docker = RuntimeContext::docker().resolve_service_endpoint(
            "temps-clickhouse",
            ServiceEndpointScheme::Http,
            8123,
            18123,
        );

        assert_eq!(host.url(None), "http://127.0.0.1:18123");
        assert_eq!(docker.url(None), "http://temps-clickhouse:8123");
    }

    #[test]
    fn process_runtime_context_reads_and_caches_the_real_environment() {
        if std::env::var_os(PROCESS_ENVIRONMENT_CHILD).is_some() {
            let first = initialize_process_runtime_context()
                .expect("configured Docker environment should initialize");
            let second = initialize_process_runtime_context()
                .expect("cached Docker environment should remain available");

            assert_eq!(first.execution_environment(), ExecutionEnvironment::Docker);
            assert_eq!(first.source(), ExecutionEnvironmentSource::Canonical);
            assert!(std::ptr::eq(first, second));
            return;
        }

        let status = run_exact_test_in_child(
            "runtime::tests::process_runtime_context_reads_and_caches_the_real_environment",
            PROCESS_ENVIRONMENT_CHILD,
            std::ffi::OsStr::new("docker"),
        );
        assert!(status.success(), "runtime environment child test failed");
    }

    #[cfg(unix)]
    #[test]
    fn process_runtime_context_rejects_and_caches_non_unicode_input() {
        if std::env::var_os(NON_UNICODE_CHILD).is_some() {
            let first = initialize_process_runtime_context()
                .expect_err("non-Unicode execution environment must fail closed");
            let second = initialize_process_runtime_context()
                .expect_err("cached non-Unicode error must remain stable");

            assert!(matches!(
                first,
                RuntimeConfigurationError::NonUnicodeExecutionEnvironment {
                    variable: EXECUTION_ENVIRONMENT_VARIABLE,
                    ..
                }
            ));
            assert_eq!(first, second);
            return;
        }

        use std::os::unix::ffi::OsStringExt;

        let invalid_value = std::ffi::OsString::from_vec(vec![0xff, 0xfe]);
        let status = run_exact_test_in_child(
            "runtime::tests::process_runtime_context_rejects_and_caches_non_unicode_input",
            NON_UNICODE_CHILD,
            &invalid_value,
        );
        assert!(status.success(), "non-Unicode runtime child test failed");
    }
}
