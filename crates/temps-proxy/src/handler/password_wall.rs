// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Password wall handler for environment password protection.
//!
//! When an environment has password protection enabled, the proxy intercepts
//! requests and shows an HTML password form. After the user enters the correct
//! password, an HMAC-signed cookie is set so subsequent requests pass through.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Cookie name for password-protected environments
pub const PASSWORD_COOKIE_NAME: &str = "_temps_pw";

/// Path the password form posts to. Reserved: it never matches a path route,
/// so for a path-routed target the form carries the routed path in
/// [`PATH_ROUTE_QUERY_PARAM`] instead (see [`path_route_lookup_path`]).
pub const PASSWORD_VERIFY_PATH: &str = "/_temps/password-verify";

/// Query parameter on the verify POST naming the request path whose path
/// route (ADR 050) the password belongs to.
const PATH_ROUTE_QUERY_PARAM: &str = "pr";

/// Cookie max age (7 days)
const COOKIE_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// HTML template for the password form
const PASSWORD_FORM_HTML: &str = include_str!("../../password_wall/password_form.html");

type HmacSha256 = Hmac<Sha256>;

/// Generate the password form HTML for a given redirect path.
///
/// `redirect_path` comes back from the client on the wrong-password
/// re-render, so it is first reduced to a same-origin absolute path
/// ([`sanitize_redirect_path`]) and then HTML-escaped like every other
/// substituted value; neither step trusts the caller to have done it.
pub fn generate_password_form_html(
    redirect_path: &str,
    show_error: bool,
    project_name: &str,
    environment_name: &str,
) -> String {
    generate_password_form_html_for_route(
        redirect_path,
        show_error,
        project_name,
        environment_name,
        None,
    )
}

/// Like [`generate_password_form_html`], for a wall rendered while serving
/// a path route (ADR 050): `routed_path` is the request path on the source
/// host that the target environment serves. The form then posts back with
/// that path so the verify request is resolved to the target environment
/// (by the proxy, from its route table) instead of the host's own.
pub fn generate_password_form_html_for_route(
    redirect_path: &str,
    show_error: bool,
    project_name: &str,
    environment_name: &str,
    routed_path: Option<&str>,
) -> String {
    let redirect_path = sanitize_redirect_path(redirect_path);
    // The client-controlled value goes in last: a redirect that spells a
    // placeholder name must not be rewritten by a later substitution.
    PASSWORD_FORM_HTML
        .replace("{{PROJECT_NAME}}", &html_escape(project_name))
        .replace("{{ENVIRONMENT_NAME}}", &html_escape(environment_name))
        .replace(
            "{{ERROR_DISPLAY}}",
            if show_error { "flex" } else { "none" },
        )
        .replace(
            "{{ERROR_INPUT_CLASS}}",
            if show_error { "input-error" } else { "" },
        )
        .replace("{{FORM_ACTION}}", &html_escape(&verify_action(routed_path)))
        .replace("{{REDIRECT_PATH}}", &html_escape(redirect_path))
}

/// The form's `action`: the verify endpoint, plus the routed request path
/// (percent-encoded) when the wall guards a path-routed target.
fn verify_action(routed_path: Option<&str>) -> String {
    match routed_path {
        Some(path) => format!(
            "{}?{}={}",
            PASSWORD_VERIFY_PATH,
            PATH_ROUTE_QUERY_PARAM,
            url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>()
        ),
        None => PASSWORD_VERIFY_PATH.to_string(),
    }
}

/// The request path whose path route a verify POST belongs to, from the
/// POST's query string. Client input: it is only ever used as a *lookup key*
/// into the route table for the request's own host, so the client can pick
/// among the prefixes that host already serves but never name an arbitrary
/// environment. Returns `None` when absent or not a same-origin path.
pub fn path_route_lookup_path(query: Option<&str>) -> Option<String> {
    let value = url::form_urlencoded::parse(query?.as_bytes())
        .find(|(key, _)| key == PATH_ROUTE_QUERY_PARAM)
        .map(|(_, value)| value.into_owned())?;
    let path = sanitize_redirect_path(&value);
    let path = path.split(['?', '#']).next().unwrap_or("/");
    // `/` is also what `sanitize_redirect_path` reduces hostile input to,
    // and no path route can match it.
    (path != "/").then(|| path.to_string())
}

/// The routed path the password form should post back, when the wall is
/// rendered for a path-routed target (`path_routed`). On the verify POST
/// itself that is the path the form carried; on any other request it is the
/// request path. `None` for a request that is not path-routed.
pub fn routed_form_path(path_routed: bool, path: &str, query: Option<&str>) -> Option<String> {
    if !path_routed {
        return None;
    }
    if path == PASSWORD_VERIFY_PATH {
        path_route_lookup_path(query)
    } else {
        Some(path.to_string())
    }
}

/// Whether an environment has its password wall switched on. Mirrors the
/// check the proxy applies to the environment it resolved for a request.
pub fn environment_has_password_wall(environment: &temps_entities::environments::Model) -> bool {
    environment
        .deployment_config
        .as_ref()
        .and_then(|config| config.security.as_ref())
        .and_then(|security| security.password_protection.as_ref())
        .is_some_and(|protection| protection.enabled)
}

/// The path route (ADR 050) serving a request, with `match_route` looking a
/// path up in the host's route table.
///
/// An ordinary request is matched by its own path. The password form's POST
/// goes to the reserved [`PASSWORD_VERIFY_PATH`], which never matches a path
/// route; it is matched by the routed path it carries instead, and only
/// kept when the proxy will handle it for the target ([`keep_path_route_for_verify`]).
pub fn resolve_request_path_route(
    method: &str,
    path: &str,
    query: Option<&str>,
    match_route: impl Fn(&str) -> Option<temps_routes::PathRoute>,
) -> Option<temps_routes::PathRoute> {
    if method == "POST" && path == PASSWORD_VERIFY_PATH {
        return path_route_lookup_path(query)
            .and_then(|routed_path| match_route(&routed_path))
            .and_then(keep_path_route_for_verify);
    }
    match_route(path)
}

/// Keep a path route found for a verify POST only when the proxy will
/// actually handle that POST on the target's behalf: the target's wall is
/// on, or the target is asleep and must be woken before it can be asked.
/// Anything else leaves the request with the host's own environment, so a
/// platform path is never forwarded into a routed service.
pub fn keep_path_route_for_verify(
    route: temps_routes::PathRoute,
) -> Option<temps_routes::PathRoute> {
    let keep = match route.target.as_ref() {
        Some(target) => target
            .environment
            .as_deref()
            .is_some_and(environment_has_password_wall),
        None => route.sleeping_environment_id.is_some(),
    };
    keep.then_some(route)
}

/// Reduce a client-supplied post-login destination to a same-origin
/// absolute path. Anything else (absolute URLs, scheme-relative `//host`,
/// backslashes, control characters, relative paths) becomes `/`, so the
/// value is safe both as a `Location` header and inside the form.
pub fn sanitize_redirect_path(redirect_path: &str) -> &str {
    temps_core::sanitize_preview_next_ref(redirect_path)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Create an HMAC-signed cookie value for a given environment ID.
///
/// The cookie value is `env_id:signature` where signature = HMAC-SHA256(env_id, secret).
/// The secret is derived from the password hash itself, so changing the password
/// invalidates all existing cookies.
pub fn create_cookie_value(environment_id: i32, password_hash: &str) -> String {
    let payload = environment_id.to_string();
    let signature = compute_hmac(&payload, password_hash);
    format!("{}:{}", payload, signature)
}

/// Validate an HMAC-signed cookie value for a given environment ID.
pub fn validate_cookie(cookie_value: &str, environment_id: i32, password_hash: &str) -> bool {
    let parts: Vec<&str> = cookie_value.splitn(2, ':').collect();
    if parts.len() != 2 {
        return false;
    }

    let payload = parts[0];
    let provided_signature = parts[1];

    // Verify the environment ID matches
    if payload != environment_id.to_string() {
        return false;
    }

    // Verify HMAC signature
    let expected_signature = compute_hmac(payload, password_hash);
    constant_time_eq(provided_signature.as_bytes(), expected_signature.as_bytes())
}

/// Verify a plaintext password against an argon2 hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    use argon2::{Argon2, PasswordHash, PasswordVerifier};
    let Ok(parsed_hash) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}

/// Build the Set-Cookie header value for a password protection cookie.
pub fn build_set_cookie_header(environment_id: i32, password_hash: &str, host: &str) -> String {
    build_set_cookie_header_for_path(environment_id, password_hash, host, "/")
}

/// Like [`build_set_cookie_header`], scoped to `path`. A path-routed target
/// (ADR 050) is unlocked on the source host only under its prefix, so its
/// cookie must not replace the source environment's own `Path=/` cookie of
/// the same name: cookies with different paths coexist.
pub fn build_set_cookie_header_for_path(
    environment_id: i32,
    password_hash: &str,
    host: &str,
    path: &str,
) -> String {
    let value = create_cookie_value(environment_id, password_hash);
    // Strip port from host for the domain
    let domain = host.split(':').next().unwrap_or(host);
    format!(
        "{}={}; Path={}; Max-Age={}; HttpOnly; SameSite=Lax; Domain={}",
        PASSWORD_COOKIE_NAME,
        value,
        cookie_path(path),
        COOKIE_MAX_AGE_SECS,
        domain
    )
}

/// `path` when it is safe as a `Path` attribute value, otherwise `/`. Route
/// prefixes are validated when stored; this keeps a header-splitting value
/// out of `Set-Cookie` even if that ever regresses.
fn cookie_path(path: &str) -> &str {
    let plain = path.starts_with('/')
        && path.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b'%')
        });
    if plain {
        path
    } else {
        "/"
    }
}

fn compute_hmac(data: &str, key: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC can take key of any size");
    mac.update(data.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ENV_ID: i32 = 42;
    const TEST_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$test_salt$test_hash_value";

    #[test]
    fn test_create_and_validate_cookie() {
        let value = create_cookie_value(TEST_ENV_ID, TEST_HASH);
        assert!(validate_cookie(&value, TEST_ENV_ID, TEST_HASH));
    }

    #[test]
    fn test_validate_cookie_wrong_env_id() {
        let value = create_cookie_value(TEST_ENV_ID, TEST_HASH);
        assert!(!validate_cookie(&value, 99, TEST_HASH));
    }

    #[test]
    fn test_validate_cookie_wrong_hash() {
        let value = create_cookie_value(TEST_ENV_ID, TEST_HASH);
        assert!(!validate_cookie(&value, TEST_ENV_ID, "different_hash"));
    }

    #[test]
    fn test_validate_cookie_tampered() {
        assert!(!validate_cookie("42:bad_signature", TEST_ENV_ID, TEST_HASH));
    }

    #[test]
    fn test_validate_cookie_malformed() {
        assert!(!validate_cookie("garbage", TEST_ENV_ID, TEST_HASH));
        assert!(!validate_cookie("", TEST_ENV_ID, TEST_HASH));
    }

    #[test]
    fn test_generate_password_form_html() {
        let html = generate_password_form_html("/some/path", false, "My Project", "staging");
        assert!(html.contains("/_temps/password-verify"));
        assert!(html.contains("/some/path"));
        assert!(html.contains("My Project"));
        assert!(html.contains("staging"));
        assert!(html.contains("display: none"));
    }

    #[test]
    fn test_generate_password_form_html_with_error() {
        let html = generate_password_form_html("/", true, "App", "production");
        assert!(html.contains("display: flex"));
        assert!(html.contains("input-error"));
    }

    #[test]
    fn test_generate_password_form_html_escapes_html() {
        let html = generate_password_form_html("/", false, "<script>xss</script>", "test");
        assert!(!html.contains("<script>xss</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn redirect_path_is_html_escaped_in_the_form() {
        // A path that closes the value attribute and injects markup must
        // come back inert: escaped, never as live HTML.
        let html = generate_password_form_html(
            "/a\"><script>alert(1)</script>",
            true,
            "App",
            "production",
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("value=\"/a&quot;&gt;&lt;script&gt;alert(1)&lt;/script&gt;\""));
    }

    #[test]
    fn redirect_path_is_reduced_to_a_same_origin_path() {
        for hostile in [
            "https://evil.example.com/",
            "//evil.example.com",
            "/\\evil.example.com",
            "javascript:alert(1)",
            "/safe\r\nLocation: //evil.example.com",
            "relative",
            "",
        ] {
            assert_eq!(sanitize_redirect_path(hostile), "/", "{hostile:?}");
            let html = generate_password_form_html(hostile, false, "App", "production");
            assert!(
                html.contains("name=\"redirect\" value=\"/\""),
                "{hostile:?}"
            );
        }
        assert_eq!(sanitize_redirect_path("/docs?q=1&x=2"), "/docs?q=1&x=2");
        let html = generate_password_form_html("/docs?q=1&x=2", false, "App", "production");
        assert!(html.contains("value=\"/docs?q=1&amp;x=2\""));
    }

    #[test]
    fn redirect_path_naming_a_placeholder_is_not_expanded() {
        let html = generate_password_form_html("/{{PROJECT_NAME}}", false, "App", "production");
        assert!(html.contains("value=\"/{{PROJECT_NAME}}\""));
    }

    #[test]
    fn test_build_set_cookie_header() {
        let header = build_set_cookie_header(TEST_ENV_ID, TEST_HASH, "example.com:443");
        assert!(header.starts_with("_temps_pw="));
        assert!(header.contains("Domain=example.com"));
        assert!(header.contains("HttpOnly"));
    }

    #[test]
    fn default_form_posts_to_the_hosts_own_verify_endpoint() {
        let html = generate_password_form_html("/", false, "App", "production");
        assert!(html.contains("action=\"/_temps/password-verify\""));
        assert!(!html.contains("{{FORM_ACTION}}"));
    }

    #[test]
    fn routed_form_carries_the_routed_path_in_the_action() {
        let html = generate_password_form_html_for_route(
            "/api/users?page=2",
            false,
            "Backend",
            "production",
            Some("/api/users"),
        );
        assert!(
            html.contains("action=\"/_temps/password-verify?pr=%2Fapi%2Fusers\""),
            "{html}"
        );
        assert!(html.contains("value=\"/api/users?page=2\""));
    }

    #[test]
    fn routed_path_cannot_break_out_of_the_action_attribute() {
        let html = generate_password_form_html_for_route(
            "/",
            false,
            "App",
            "production",
            Some("/a\"><script>alert(1)</script>{{PROJECT_NAME}}"),
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(!html.contains("action=\"/_temps/password-verify?pr=/a\""));
        assert!(html.contains("%22%3E%3Cscript%3E"));
    }

    #[test]
    fn lookup_path_round_trips_through_the_form_action() {
        let action = verify_action(Some("/api/v2/users"));
        let query = action.split_once('?').map(|(_, query)| query);
        assert_eq!(
            path_route_lookup_path(query).as_deref(),
            Some("/api/v2/users")
        );
    }

    #[test]
    fn lookup_path_drops_query_and_fragment() {
        assert_eq!(
            path_route_lookup_path(Some("pr=%2Fapi%2Fusers%3Fa%3D1%23x")).as_deref(),
            Some("/api/users")
        );
    }

    #[test]
    fn lookup_path_rejects_missing_hostile_and_root_values() {
        assert_eq!(path_route_lookup_path(None), None);
        assert_eq!(path_route_lookup_path(Some("")), None);
        assert_eq!(path_route_lookup_path(Some("other=%2Fapi")), None);
        assert_eq!(path_route_lookup_path(Some("pr=%2F")), None);
        for hostile in [
            "pr=https%3A%2F%2Fevil.example.com%2Fapi",
            "pr=%2F%2Fevil.example.com",
            "pr=api",
            "pr=javascript%3Aalert(1)",
        ] {
            assert_eq!(path_route_lookup_path(Some(hostile)), None, "{hostile}");
        }
    }

    #[test]
    fn path_scoped_cookie_does_not_use_the_root_path() {
        let header =
            build_set_cookie_header_for_path(TEST_ENV_ID, TEST_HASH, "example.com:443", "/api");
        assert!(header.contains("Path=/api;"), "{header}");
        assert!(header.contains("Domain=example.com"));
        assert!(header.contains(&format!("_temps_pw={}:", TEST_ENV_ID)));
        // The default builder keeps the environment-wide cookie.
        let root = build_set_cookie_header(TEST_ENV_ID, TEST_HASH, "example.com");
        assert!(root.contains("Path=/;"), "{root}");
    }

    #[test]
    fn cookie_path_refuses_values_that_could_split_the_header() {
        for hostile in [
            "/api; Domain=evil.example.com",
            "/a\r\nSet-Cookie: x=y",
            "api",
            "",
        ] {
            let header = build_set_cookie_header_for_path(1, TEST_HASH, "example.com", hostile);
            assert!(header.contains("Path=/;"), "{hostile:?} -> {header}");
        }
    }

    fn walled_environment(enabled: Option<bool>) -> temps_entities::environments::Model {
        let mut environment: temps_entities::environments::Model =
            serde_json::from_value(serde_json::json!({
                "id": 20, "name": "production", "slug": "production",
                "subdomain": "production", "last_deployment": null, "host": "",
                "upstreams": [], "created_at": "2026-09-28T00:00:00Z",
                "updated_at": "2026-09-28T00:00:00Z", "project_id": 2,
                "current_deployment_id": null, "branch": null, "deleted_at": null,
                "deployment_config": null, "is_preview": false, "protected": false,
                "sleeping": false, "attack_mode": null, "force_https": null,
                "last_activity_at": null
            }))
            .expect("environment fixture");
        environment.deployment_config =
            enabled.map(
                |enabled| temps_entities::deployment_config::DeploymentConfig {
                    security: Some(temps_entities::deployment_config::SecurityConfig {
                        password_protection: Some(
                            temps_entities::deployment_config::PasswordProtectionConfig {
                                enabled,
                                password_hash: TEST_HASH.to_string(),
                            },
                        ),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            );
        environment
    }

    fn route_to(
        environment: Option<temps_entities::environments::Model>,
        sleeping_environment_id: Option<i32>,
    ) -> temps_routes::PathRoute {
        let target = environment.map(|environment| temps_routes::RouteInfo {
            backend: temps_routes::BackendType::StaticDir {
                path: "/tmp/backend".to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: Some(std::sync::Arc::new(environment)),
            deployment: None,
            cert_eligible: false,
        });
        temps_routes::PathRoute {
            prefix: "/api".to_string(),
            strip_prefix: false,
            target_service: "backend".to_string(),
            target,
            sleeping_environment_id,
        }
    }

    /// `web.example.com` (environment 10) serves `/api` from `target`.
    fn table_routing_api_to(
        target: temps_entities::environments::Model,
    ) -> temps_routes::CachedPeerTable {
        let table = temps_routes::CachedPeerTable::new(std::sync::Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        ));
        let mut source = walled_environment(None);
        source.id = 10;
        source.project_id = 1;
        table.insert_route_for_test(
            "web.example.com",
            temps_routes::RouteInfo {
                backend: temps_routes::BackendType::StaticDir {
                    path: "/tmp/frontend".to_string(),
                },
                redirect_to: None,
                status_code: None,
                project: None,
                environment: Some(std::sync::Arc::new(source)),
                deployment: None,
                cert_eligible: false,
            },
        );
        let mut route = route_to(Some(target), None);
        route.prefix = "/api".to_string();
        table.insert_path_routes_for_test(10, vec![route]);
        table
    }

    #[test]
    fn verify_post_resolves_the_target_the_form_was_rendered_for() {
        let table = table_routing_api_to(walled_environment(Some(true)));
        let lookup = |path: &str| table.match_path_route("web.example.com", path);

        let route = resolve_request_path_route(
            "POST",
            PASSWORD_VERIFY_PATH,
            Some("pr=%2Fapi%2Fusers"),
            lookup,
        )
        .expect("verify POST for a routed path resolves to the target");
        assert_eq!(route.prefix, "/api");
        assert_eq!(
            route
                .target
                .and_then(|target| target.environment)
                .map(|environment| environment.id),
            Some(20)
        );
    }

    #[test]
    fn verify_post_for_an_unrouted_path_stays_with_the_host() {
        let table = table_routing_api_to(walled_environment(Some(true)));
        let lookup = |path: &str| table.match_path_route("web.example.com", path);

        for query in [
            None,
            Some("pr=%2Fother"),
            Some("pr=%2F"),
            Some("pr=%2F%2Fevil"),
        ] {
            assert!(
                resolve_request_path_route("POST", PASSWORD_VERIFY_PATH, query, lookup).is_none(),
                "{query:?}"
            );
        }
        // Another host cannot borrow this host's rules.
        let other = |path: &str| table.match_path_route("other.example.com", path);
        assert!(
            resolve_request_path_route("POST", PASSWORD_VERIFY_PATH, Some("pr=%2Fapi"), other)
                .is_none()
        );
    }

    #[test]
    fn verify_lookup_only_applies_to_the_verify_post() {
        let table = table_routing_api_to(walled_environment(Some(true)));
        let lookup = |path: &str| table.match_path_route("web.example.com", path);

        // A GET of the reserved path is not rerouted by a `pr` parameter.
        assert!(
            resolve_request_path_route("GET", PASSWORD_VERIFY_PATH, Some("pr=%2Fapi"), lookup)
                .is_none()
        );
        // Ordinary requests match by their own path, `pr` or not.
        let route = resolve_request_path_route("GET", "/api/users", None, lookup)
            .expect("ordinary path route");
        assert_eq!(route.prefix, "/api");
        assert!(resolve_request_path_route("GET", "/", Some("pr=%2Fapi"), lookup).is_none());
    }

    #[test]
    fn verify_post_for_an_unwalled_target_is_not_forwarded_into_it() {
        let table = table_routing_api_to(walled_environment(None));
        let lookup = |path: &str| table.match_path_route("web.example.com", path);
        assert!(resolve_request_path_route(
            "POST",
            PASSWORD_VERIFY_PATH,
            Some("pr=%2Fapi%2Fusers"),
            lookup
        )
        .is_none());
    }

    #[test]
    fn routed_form_path_follows_the_request_or_the_verify_query() {
        assert_eq!(routed_form_path(false, "/api/users", None), None);
        assert_eq!(
            routed_form_path(true, "/api/users", None).as_deref(),
            Some("/api/users")
        );
        assert_eq!(
            routed_form_path(true, PASSWORD_VERIFY_PATH, Some("pr=%2Fapi%2Fusers")).as_deref(),
            Some("/api/users")
        );
        assert_eq!(routed_form_path(true, PASSWORD_VERIFY_PATH, None), None);
    }

    #[test]
    fn verify_post_is_routed_to_a_walled_target() {
        let route = route_to(Some(walled_environment(Some(true))), None);
        assert!(keep_path_route_for_verify(route).is_some());
    }

    #[test]
    fn verify_post_is_not_routed_to_an_unwalled_target() {
        for enabled in [None, Some(false)] {
            let route = route_to(Some(walled_environment(enabled)), None);
            assert!(keep_path_route_for_verify(route).is_none(), "{enabled:?}");
        }
    }

    #[test]
    fn verify_post_wakes_a_sleeping_target_and_drops_an_undeployed_one() {
        assert!(keep_path_route_for_verify(route_to(None, Some(20))).is_some());
        assert!(keep_path_route_for_verify(route_to(None, None)).is_none());
    }
}
