// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use bollard::errors::Error;
use bollard::models::NetworkCreateRequest;
use bollard::Docker;
use std::collections::HashMap;

pub(crate) const BRIDGE_GATEWAY_MODE_IPV4_OPTION: &str =
    "com.docker.network.bridge.gateway_mode_ipv4";
pub(crate) const BRIDGE_INHIBIT_IPV4_OPTION: &str = "com.docker.network.bridge.inhibit_ipv4";
const ISOLATED_GATEWAY_MODE: &str = "isolated";

/// Configure an internal bridge so the Docker host has no address on it.
///
/// Docker 28 added the explicit `isolated` gateway mode. Older daemons provide
/// the same IPv4 isolation through `inhibit_ipv4`; network creation retries
/// with that compatible spelling only when the daemon rejects the newer one.
pub(crate) fn with_host_isolation(mut request: NetworkCreateRequest) -> NetworkCreateRequest {
    request.driver = Some("bridge".to_string());
    request.internal = Some(true);
    request.enable_ipv6 = Some(false);
    request.options.get_or_insert_with(HashMap::new).insert(
        BRIDGE_GATEWAY_MODE_IPV4_OPTION.to_string(),
        ISOLATED_GATEWAY_MODE.to_string(),
    );
    request
}

fn with_legacy_host_isolation(mut request: NetworkCreateRequest) -> NetworkCreateRequest {
    let options = request.options.get_or_insert_with(HashMap::new);
    options.remove(BRIDGE_GATEWAY_MODE_IPV4_OPTION);
    options.insert(BRIDGE_INHIBIT_IPV4_OPTION.to_string(), "true".to_string());
    request
}

pub(crate) fn has_host_isolation(options: Option<&HashMap<String, String>>) -> bool {
    options.is_some_and(|options| {
        options
            .get(BRIDGE_GATEWAY_MODE_IPV4_OPTION)
            .is_some_and(|value| value == ISOLATED_GATEWAY_MODE)
            || options
                .get(BRIDGE_INHIBIT_IPV4_OPTION)
                .is_some_and(|value| value == "true")
    })
}

fn rejects_isolated_gateway_mode(error: &Error) -> bool {
    let Error::DockerResponseServerError {
        status_code: 400,
        message,
    } = error
    else {
        return false;
    };

    let message = message.to_ascii_lowercase();
    message.contains(BRIDGE_GATEWAY_MODE_IPV4_OPTION)
        && (message.contains("unknown gateway mode")
            || message.contains("unknown option")
            || message.contains("invalid value"))
}

fn compatibility_request(
    error: &Error,
    request: NetworkCreateRequest,
) -> Option<NetworkCreateRequest> {
    rejects_isolated_gateway_mode(error).then(|| with_legacy_host_isolation(request))
}

pub(crate) async fn create_host_isolated_network(
    docker: &Docker,
    request: NetworkCreateRequest,
) -> Result<(), Error> {
    let request = with_host_isolation(request);
    match docker.create_network(request.clone()).await {
        Ok(_) => Ok(()),
        Err(error) => {
            let Some(compatibility_request) = compatibility_request(&error, request) else {
                return Err(error);
            };
            tracing::warn!(
                network = %compatibility_request.name,
                modern_error = %error,
                "Docker does not support isolated gateway mode; using the equivalent internal inhibit_ipv4 policy"
            );
            docker
                .create_network(compatibility_request)
                .await
                .map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_existing_option() -> NetworkCreateRequest {
        NetworkCreateRequest {
            name: "sandbox-network".to_string(),
            internal: Some(true),
            options: Some(HashMap::from([(
                "com.docker.network.bridge.enable_ip_masquerade".to_string(),
                "false".to_string(),
            )])),
            ..Default::default()
        }
    }

    #[test]
    fn modern_and_legacy_requests_preserve_host_isolation() {
        let modern = with_host_isolation(request_with_existing_option());
        assert_eq!(modern.driver.as_deref(), Some("bridge"));
        assert_eq!(modern.internal, Some(true));
        assert_eq!(modern.enable_ipv6, Some(false));
        assert!(has_host_isolation(modern.options.as_ref()));
        assert_eq!(
            modern
                .options
                .as_ref()
                .and_then(|options| options.get("com.docker.network.bridge.enable_ip_masquerade"))
                .map(String::as_str),
            Some("false")
        );

        let legacy = with_legacy_host_isolation(modern);
        assert_eq!(legacy.name, "sandbox-network");
        assert_eq!(legacy.driver.as_deref(), Some("bridge"));
        assert_eq!(legacy.internal, Some(true));
        assert_eq!(legacy.enable_ipv6, Some(false));
        assert!(has_host_isolation(legacy.options.as_ref()));
        assert_eq!(
            legacy
                .options
                .as_ref()
                .and_then(|options| options.get(BRIDGE_INHIBIT_IPV4_OPTION))
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            legacy
                .options
                .as_ref()
                .and_then(|options| options.get("com.docker.network.bridge.enable_ip_masquerade"))
                .map(String::as_str),
            Some("false")
        );
        assert!(!legacy
            .options
            .as_ref()
            .is_some_and(|options| options.contains_key(BRIDGE_GATEWAY_MODE_IPV4_OPTION)));
    }

    #[test]
    fn production_daemon_error_selects_the_secure_compatibility_path() {
        let unsupported = Error::DockerResponseServerError {
            status_code: 400,
            message: format!(
                "failed to parse {BRIDGE_GATEWAY_MODE_IPV4_OPTION} value: isolated (unknown gateway mode isolated)"
            ),
        };
        let unrelated = Error::DockerResponseServerError {
            status_code: 400,
            message: "invalid subnet allocation".to_string(),
        };
        let older_daemon = Error::DockerResponseServerError {
            status_code: 400,
            message: format!("unknown option {BRIDGE_GATEWAY_MODE_IPV4_OPTION}"),
        };
        let server_failure = Error::DockerResponseServerError {
            status_code: 500,
            message: format!(
                "failed to parse {BRIDGE_GATEWAY_MODE_IPV4_OPTION} value: isolated (unknown gateway mode isolated)"
            ),
        };

        let fallback = compatibility_request(
            &unsupported,
            with_host_isolation(request_with_existing_option()),
        )
        .expect("the reported unsupported mode must use the secure compatibility request");
        assert!(has_host_isolation(fallback.options.as_ref()));
        assert_eq!(
            fallback
                .options
                .as_ref()
                .and_then(|options| options.get(BRIDGE_INHIBIT_IPV4_OPTION))
                .map(String::as_str),
            Some("true")
        );
        assert!(compatibility_request(&older_daemon, request_with_existing_option()).is_some());
        assert!(compatibility_request(&unrelated, request_with_existing_option()).is_none());
        assert!(compatibility_request(&server_failure, request_with_existing_option()).is_none());
    }

    #[test]
    fn networks_without_either_supported_isolation_option_fail_policy_validation() {
        assert!(!has_host_isolation(Some(&HashMap::new())));
        assert!(!has_host_isolation(None));
        for options in [
            HashMap::from([(
                BRIDGE_GATEWAY_MODE_IPV4_OPTION.to_string(),
                "nat".to_string(),
            )]),
            HashMap::from([(BRIDGE_INHIBIT_IPV4_OPTION.to_string(), "false".to_string())]),
        ] {
            assert!(!has_host_isolation(Some(&options)));
        }
    }
}
