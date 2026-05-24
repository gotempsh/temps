//! Live OIDC discovery tests against real IdPs.
//!
//! These hit the public internet, so they only run when
//! `TEMPS_RUN_LIVE_OIDC_TESTS=1` is set. They reproduce real-world
//! discovery quirks (Auth0's trailing-slash issuer, etc.) so we can
//! regression-guard the error surface in `oidc_service.rs`.
//!
//! Run with:
//!     TEMPS_RUN_LIVE_OIDC_TESTS=1 cargo test --test oidc_discovery_live -p temps-auth -- --nocapture

use openidconnect::core::CoreProviderMetadata;
use openidconnect::IssuerUrl;

fn live_enabled() -> bool {
    std::env::var("TEMPS_RUN_LIVE_OIDC_TESTS").ok().as_deref() == Some("1")
}

/// Mirrors the production client config in
/// `OidcService::new`: explicit timeout + no redirect following (so
/// a malicious IdP can't 302 us to an internal URL on the first hop).
fn build_test_http_client() -> reqwest::Client {
    reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("test reqwest client should build")
}

fn format_error_chain<E: std::error::Error>(err: &E) -> String {
    let mut out = format!("{err}");
    let mut src: Option<&dyn std::error::Error> = err.source();
    while let Some(cause) = src {
        out.push_str(&format!("\n  caused by: {cause}"));
        src = cause.source();
    }
    out
}

#[tokio::test]
async fn auth0_kungfusoftware_discovery_succeeds_with_trailing_slash() {
    if !live_enabled() {
        eprintln!("skipping live OIDC test (set TEMPS_RUN_LIVE_OIDC_TESTS=1 to enable)");
        return;
    }

    // Auth0 returns `issuer: "https://kungfusoftware.eu.auth0.com/"` (note
    // the trailing slash). openidconnect's discovery validation requires
    // EXACT string equality, so we must pass the issuer URL with the same
    // trailing slash or it will fail with DiscoveryError::Validation.
    let issuer =
        IssuerUrl::new("https://kungfusoftware.eu.auth0.com/".to_string()).expect("valid issuer");
    let http_client = build_test_http_client();

    let result = CoreProviderMetadata::discover_async(issuer, &http_client).await;

    match result {
        Ok(meta) => {
            println!("OK: issuer={}", meta.issuer().as_str());
            assert_eq!(
                meta.issuer().as_str(),
                "https://kungfusoftware.eu.auth0.com/"
            );
        }
        Err(e) => panic!("discovery failed: {}", format_error_chain(&e)),
    }
}

#[tokio::test]
async fn auth0_kungfusoftware_discovery_fails_without_trailing_slash() {
    if !live_enabled() {
        eprintln!("skipping live OIDC test (set TEMPS_RUN_LIVE_OIDC_TESTS=1 to enable)");
        return;
    }

    // This is what the *old* `normalize_issuer_url` used to do (strip
    // the trailing slash). Keeping the failure case as a guard: if
    // someone re-introduces the slash strip, this test will be the
    // canary that catches it.
    let issuer =
        IssuerUrl::new("https://kungfusoftware.eu.auth0.com".to_string()).expect("valid issuer");
    let http_client = build_test_http_client();

    let result = CoreProviderMetadata::discover_async(issuer, &http_client).await;

    let err = match result {
        Ok(meta) => {
            // If this ever starts succeeding, Auth0 changed their issuer
            // format and we can relax the trailing-slash workaround.
            panic!(
                "expected validation failure but discovery succeeded: issuer={}",
                meta.issuer().as_str()
            );
        }
        Err(e) => e,
    };

    let chain = format_error_chain(&err);
    println!("got expected error:\n{chain}");
    assert!(
        chain.contains("unexpected issuer URI") && chain.contains("expected"),
        "expected an issuer-mismatch validation error, got:\n{chain}"
    );
}
