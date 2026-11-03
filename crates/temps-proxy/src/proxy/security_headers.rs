// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Security response headers: the project or global preset, and the proxy-owned headers no application may send.

use super::*;

/// Say a response came from this proxy. Upstream responses get this in
/// `upstream_response_filter`; anything the proxy answers itself bypasses that
/// filter, so it marks its own. Monitors and the reachability check read it.
pub(super) fn mark_served_by_proxy(response: &mut ResponseHeader) -> Result<()> {
    response.insert_header("X-Temps-Proxy-Probe-Capable", "1")?;
    Ok(())
}

pub(super) fn strip_proxy_owned_response_headers(response: &mut ResponseHeader) {
    // Applications must not be able to impersonate the pre-upstream redirect
    // used by managed monitors to decide whether a local TLS follow-up is safe.
    response.remove_header("X-Temps-Proxy-Https-Redirect");
    response.remove_header("X-Temps-Proxy-Probe-Capable");
    // Monitors read `x-temps-gate` as the gate's own answer (ADR 065 D10),
    // so no app, gated or not, may send it.
    response.remove_header(temps_core::gate::GATE_HEADER);
}

impl LoadBalancer {
    /// Apply security headers from project settings or global config
    ///
    /// Attempts to use project-level security settings first (via temps-routes),
    /// then falls back to global config service settings if project is unavailable
    pub(super) async fn apply_security_headers(
        &self,
        response: &mut ResponseHeader,
        project: Option<&projects::Model>,
    ) -> Result<()> {
        use temps_entities::deployment_config::SecurityHeadersConfig;

        // Map preset names to default header values
        fn get_preset_headers(preset: &str) -> SecurityHeadersConfig {
            match preset.to_lowercase().as_str() {
                "strict" => SecurityHeadersConfig {
                    preset: Some("strict".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'".to_string()
                    ),
                    x_frame_options: Some("DENY".to_string()),
                    strict_transport_security: Some("max-age=31536000; includeSubDomains; preload".to_string()),
                    referrer_policy: Some("strict-origin-when-cross-origin".to_string()),
                },
                "moderate" => SecurityHeadersConfig {
                    preset: Some("moderate".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self' https:; frame-ancestors 'self'".to_string()
                    ),
                    x_frame_options: Some("SAMEORIGIN".to_string()),
                    strict_transport_security: Some("max-age=31536000; includeSubDomains".to_string()),
                    referrer_policy: Some("no-referrer-when-downgrade".to_string()),
                },
                "permissive" => SecurityHeadersConfig {
                    preset: Some("permissive".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' https:; style-src 'self' 'unsafe-inline' https:; img-src 'self' data: https:; font-src 'self' data: https:; connect-src 'self' https:; frame-ancestors *".to_string()
                    ),
                    x_frame_options: Some("ALLOW-FROM *".to_string()),
                    strict_transport_security: Some("max-age=31536000".to_string()),
                    referrer_policy: Some("origin".to_string()),
                },
                "disabled" => SecurityHeadersConfig {
                    preset: Some("disabled".to_string()),
                    content_security_policy: None,
                    x_frame_options: None,
                    strict_transport_security: None,
                    referrer_policy: None,
                },
                _ => SecurityHeadersConfig {
                    preset: Some(preset.to_string()),
                    content_security_policy: None,
                    x_frame_options: None,
                    strict_transport_security: None,
                    referrer_policy: None,
                },
            }
        }

        // Try to get security headers from project configuration first
        // Returns: None = no config (should check global), Some(config) = explicit config from project
        let (project_has_explicit_config, headers_config) = if let Some(proj) = project {
            debug!(
                "Applying security headers for project id={}, slug={}",
                proj.id, proj.slug
            );
            if let Some(ref deploy_config) = proj.deployment_config {
                debug!(
                    "Project {} has deployment_config, security field: {}",
                    proj.id,
                    deploy_config.security.is_some()
                );
                if let Some(ref security) = deploy_config.security {
                    debug!(
                        "Security config present: enabled={}, headers={}, rate_limiting={}, attack_mode={}",
                        security.enabled.unwrap_or(true),
                        security.headers.is_some(),
                        security.rate_limiting.is_some(),
                        security.attack_mode.is_some()
                    );

                    // Check if security is explicitly disabled at project level
                    if security.enabled == Some(false) {
                        debug!("Security headers are explicitly disabled at project level - skipping global fallback");
                        return Ok(());
                    }

                    if let Some(ref headers) = security.headers {
                        // Check if we have a preset but no individual headers configured
                        let has_preset = headers.preset.is_some();
                        let has_individual_headers = headers.content_security_policy.is_some()
                            || headers.x_frame_options.is_some()
                            || headers.strict_transport_security.is_some()
                            || headers.referrer_policy.is_some();

                        // Check if preset is "disabled"
                        let preset_disabled = has_preset
                            && headers.preset.as_ref().map(|p| p.to_lowercase())
                                == Some("disabled".to_string());

                        if preset_disabled {
                            debug!("Project has security headers preset set to 'disabled' - skipping global fallback");
                            return Ok(());
                        }

                        if has_preset && !has_individual_headers {
                            // Use preset to generate default headers
                            if let Some(preset_name) = headers.preset.as_ref() {
                                debug!(
                                    "Using preset '{}' to generate security headers from project config",
                                    preset_name
                                );
                                (true, Some(get_preset_headers(preset_name)))
                            } else {
                                // has_preset was true but preset is None — should not happen,
                                // fall through to global config
                                (false, None)
                            }
                        } else if has_individual_headers {
                            // Use individual headers as configured
                            debug!(
                                "Using custom security headers from project: preset={:?}, csp={}, x_frame={}, hsts={}, referrer={}",
                                headers.preset,
                                headers.content_security_policy.is_some(),
                                headers.x_frame_options.is_some(),
                                headers.strict_transport_security.is_some(),
                                headers.referrer_policy.is_some()
                            );
                            (true, Some(headers.clone()))
                        } else {
                            // No preset and no individual headers - project has config but empty, don't fall back to global
                            debug!("Project has security config but no headers or preset configured - skipping global fallback");
                            (true, None)
                        }
                    } else {
                        debug!("Project has security config but no headers configured (headers field is None) - allowing global fallback");
                        (false, None)
                    }
                } else {
                    debug!("Project has deployment_config but no security config (security field is None) - allowing global fallback");
                    (false, None)
                }
            } else {
                debug!("Project {} has no deployment_config field (is None) - allowing global fallback", proj.id);
                (false, None)
            }
        } else {
            debug!("No project context available for security headers - allowing global fallback");
            (false, None)
        };

        // If project didn't have explicit config, check global settings
        let headers_config = if !project_has_explicit_config && headers_config.is_none() {
            debug!("No explicit project-level security headers, checking global settings");
            match self.config_service.get_settings().await {
                Ok(settings) => {
                    let headers = &settings.security_headers;
                    if !headers.enabled {
                        debug!("Security headers are disabled in global settings");
                        return Ok(());
                    }
                    debug!("Using global security headers: preset={}", headers.preset);
                    Some(SecurityHeadersConfig {
                        preset: Some(headers.preset.clone()),
                        content_security_policy: headers.content_security_policy.clone(),
                        x_frame_options: Some(headers.x_frame_options.clone()),
                        strict_transport_security: Some(headers.strict_transport_security.clone()),
                        referrer_policy: Some(headers.referrer_policy.clone()),
                    })
                }
                Err(e) => {
                    warn!("Failed to get settings for security headers: {}", e);
                    return Ok(()); // Don't fail the request if we can't get settings
                }
            }
        } else {
            headers_config
        };

        // Apply headers from configuration
        if let Some(config) = headers_config {
            let mut headers_applied = Vec::new();

            // Apply Content-Security-Policy
            if let Some(ref csp) = config.content_security_policy {
                if !csp.is_empty() {
                    if let Err(e) = response.insert_header("Content-Security-Policy", csp) {
                        warn!("Failed to set Content-Security-Policy header: {}", e);
                    } else {
                        headers_applied.push("Content-Security-Policy");
                    }
                }
            }

            // Apply X-Frame-Options
            if let Some(ref x_frame) = config.x_frame_options {
                if !x_frame.is_empty() {
                    if let Err(e) = response.insert_header("X-Frame-Options", x_frame) {
                        warn!("Failed to set X-Frame-Options header: {}", e);
                    } else {
                        headers_applied.push("X-Frame-Options");
                    }
                }
            }

            // Apply Strict-Transport-Security
            if let Some(ref hsts) = config.strict_transport_security {
                if !hsts.is_empty() {
                    if let Err(e) = response.insert_header("Strict-Transport-Security", hsts) {
                        warn!("Failed to set Strict-Transport-Security header: {}", e);
                    } else {
                        headers_applied.push("Strict-Transport-Security");
                    }
                }
            }

            // Apply Referrer-Policy
            if let Some(ref policy) = config.referrer_policy {
                if !policy.is_empty() {
                    if let Err(e) = response.insert_header("Referrer-Policy", policy) {
                        warn!("Failed to set Referrer-Policy header: {}", e);
                    } else {
                        headers_applied.push("Referrer-Policy");
                    }
                }
            }

            if headers_applied.is_empty() {
                debug!("No security headers to apply (all configs empty)");
            } else {
                debug!(
                    "Applied {} security headers: {:?}",
                    headers_applied.len(),
                    headers_applied
                );
            }
        } else {
            debug!("No security headers configuration available");
        }

        Ok(())
    }
}

#[cfg(test)]
mod security_headers_tests {
    use super::super::static_serving::static_path_route_redirect_tests::load_balancer;
    use super::strip_proxy_owned_response_headers;
    use pingora_http::ResponseHeader;
    use temp_dir::TempDir;
    use temps_entities::deployment_config::{
        DeploymentConfig, SecurityConfig, SecurityHeadersConfig,
    };
    use temps_entities::projects;

    fn project_with_security(security: SecurityConfig) -> projects::Model {
        let mut project: projects::Model = serde_json::from_value(serde_json::json!({
            "id": 1, "name": "app", "slug": "app", "repo_name": "app",
            "repo_owner": "example", "directory": ".", "main_branch": "main",
            "preset": "dockerfile", "created_at": chrono::Utc::now(),
            "updated_at": chrono::Utc::now(), "pull_only_root_directory": false,
            "is_deleted": false, "is_public_repo": false, "attack_mode": false,
            "shield_mode": "log",
            "ai_write_actions_enabled": false, "error_source_context_enabled": false,
            "vulnerability_scanning_enabled": false, "enable_preview_environments": false,
            "preview_envs_on_demand": false, "preview_envs_idle_timeout_seconds": 300,
            "preview_envs_wake_timeout_seconds": 30, "source_type": "docker_image",
            "project_type": temps_entities::types::ProjectType::Server,
            "hosting": temps_entities::types::Hosting::Hosted,
            "cross_project_trace_sharing": true, "cloud_telemetry_fidelity": "metered",
            "cloud_telemetry_attribute_allowlist": [], "cloud_telemetry_write_mode": "local",
            "cloud_analytics_write_mode": "local"
        }))
        .expect("test project fixture deserializes");
        project.deployment_config = Some(DeploymentConfig {
            security: Some(security),
            ..DeploymentConfig::default()
        });
        project
    }

    fn header<'a>(response: &'a ResponseHeader, name: &str) -> Option<&'a str> {
        response.headers.get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn a_project_preset_sets_its_headers_without_reading_settings() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let lb = load_balancer(dir.path())?;
        let project = project_with_security(SecurityConfig {
            headers: Some(SecurityHeadersConfig {
                preset: Some("strict".to_string()),
                content_security_policy: None,
                x_frame_options: None,
                strict_transport_security: None,
                referrer_policy: None,
            }),
            ..SecurityConfig::default()
        });
        let mut response = ResponseHeader::build(200, None)?;

        lb.apply_security_headers(&mut response, Some(&project))
            .await?;

        assert_eq!(header(&response, "X-Frame-Options"), Some("DENY"));
        assert_eq!(
            header(&response, "Referrer-Policy"),
            Some("strict-origin-when-cross-origin")
        );
        assert!(header(&response, "Content-Security-Policy")
            .is_some_and(|csp| csp.contains("frame-ancestors 'none'")));
        assert!(header(&response, "Strict-Transport-Security").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn a_project_that_disables_security_gets_no_headers() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let lb = load_balancer(dir.path())?;
        let project = project_with_security(SecurityConfig {
            enabled: Some(false),
            ..SecurityConfig::default()
        });
        let mut response = ResponseHeader::build(200, None)?;

        lb.apply_security_headers(&mut response, Some(&project))
            .await?;

        assert!(header(&response, "X-Frame-Options").is_none());
        assert!(header(&response, "Content-Security-Policy").is_none());
        Ok(())
    }

    #[test]
    fn applications_cannot_send_proxy_owned_headers() -> anyhow::Result<()> {
        let mut response = ResponseHeader::build(200, None)?;
        response.insert_header("X-Temps-Proxy-Https-Redirect", "1")?;
        response.insert_header("X-Temps-Proxy-Probe-Capable", "1")?;
        response.insert_header(temps_core::gate::GATE_HEADER, "allow")?;
        response.insert_header("X-App", "kept")?;

        strip_proxy_owned_response_headers(&mut response);

        assert!(header(&response, "X-Temps-Proxy-Https-Redirect").is_none());
        assert!(header(&response, "X-Temps-Proxy-Probe-Capable").is_none());
        assert!(header(&response, temps_core::gate::GATE_HEADER).is_none());
        assert_eq!(header(&response, "X-App"), Some("kept"));
        Ok(())
    }
}
