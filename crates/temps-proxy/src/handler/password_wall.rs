//! Password wall handler for environment password protection.
//!
//! When an environment has password protection enabled, the proxy intercepts
//! requests and shows an HTML password form. After the user enters the correct
//! password, an HMAC-signed cookie is set so subsequent requests pass through.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Cookie name for password-protected environments
pub const PASSWORD_COOKIE_NAME: &str = "_temps_pw";

/// Cookie max age (7 days)
const COOKIE_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// HTML template for the password form
const PASSWORD_FORM_HTML: &str = include_str!("../../password_wall/password_form.html");

type HmacSha256 = Hmac<Sha256>;

/// Generate the password form HTML for a given redirect path.
pub fn generate_password_form_html(
    redirect_path: &str,
    show_error: bool,
    project_name: &str,
    environment_name: &str,
) -> String {
    PASSWORD_FORM_HTML
        .replace("{{REDIRECT_PATH}}", redirect_path)
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
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    let value = create_cookie_value(environment_id, password_hash);
    // Strip port from host for the domain
    let domain = host.split(':').next().unwrap_or(host);
    format!(
        "{}={}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax; Domain={}",
        PASSWORD_COOKIE_NAME, value, COOKIE_MAX_AGE_SECS, domain
    )
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
    fn test_build_set_cookie_header() {
        let header = build_set_cookie_header(TEST_ENV_ID, TEST_HASH, "example.com:443");
        assert!(header.starts_with("_temps_pw="));
        assert!(header.contains("Domain=example.com"));
        assert!(header.contains("HttpOnly"));
    }
}
