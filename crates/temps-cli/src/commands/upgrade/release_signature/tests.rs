// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::test_sigstore::TestSigstore;
use super::*;

/// Real bundles published by this repository's release workflow, with the
/// SHA-256 of the tarball each one signs (from the release's `.sha256`).
/// Verified offline against the embedded trusted root, they prove the
/// identity matcher accepts what real stable/beta and nightly releases carry.
const BETA_TAG: &str = "v0.1.0-beta.62";
const BETA_BUNDLE: &str =
    include_str!("fixtures/v0.1.0-beta.62-temps-linux-amd64.tar.gz.sigstore.json");
const BETA_SHA256: &str = "ec3cea068160e5b4a318173cb1d23cd28960c7381570812ac7e087444bfd0c53";
const NIGHTLY_TAG: &str = "v0.1.0-nightly.20261118.0bb170230";
const NIGHTLY_BUNDLE: &str = include_str!(
    "fixtures/v0.1.0-nightly.20261118.0bb170230-temps-linux-amd64.tar.gz.sigstore.json"
);
const NIGHTLY_SHA256: &str = "184eb5aa76078c7eeb7cba0612e1316076ddfffb73ae8977a81356e98fe378b6";
const ASSET: &str = "temps-linux-amd64.tar.gz";

fn embedded() -> ReleaseTrust {
    ReleaseTrust::embedded().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Flip the last hex digit: a digest of different bytes.
fn tampered(sha256_hex: &str) -> String {
    let mut digest = sha256_hex.to_string();
    let last = if digest.ends_with('0') { "1" } else { "0" };
    digest.replace_range(63.., last);
    digest
}

fn test_release(tag: &str, assets: &[(&str, String)]) -> GitHubRelease {
    serde_json::from_value(serde_json::json!({
        "tag_name": tag,
        "prerelease": true,
        "draft": false,
        "html_url": format!("https://example.invalid/releases/{tag}"),
        "assets": assets
            .iter()
            .map(|(name, url)| serde_json::json!({
                "name": name, "browser_download_url": url, "size": 1
            }))
            .collect::<Vec<_>>(),
    }))
    .unwrap()
}

#[test]
fn identity_is_the_release_workflow_at_the_exact_tag() {
    assert_eq!(
        expected_release_identity("v1.2.3"),
        "https://github.com/gotempsh/temps/.github/workflows/release.yml@refs/tags/v1.2.3"
    );
    assert_eq!(
        bundle_asset_name(ASSET),
        "temps-linux-amd64.tar.gz.sigstore.json"
    );
}

#[test]
fn real_beta_bundle_verifies_against_the_embedded_root() {
    let verified = verify_release_bundle(&embedded(), BETA_TAG, ASSET, BETA_SHA256, BETA_BUNDLE)
        .expect("a published beta bundle must verify");
    assert_eq!(verified.identity, expected_release_identity(BETA_TAG));
    assert_eq!(verified.issuer, GITHUB_ACTIONS_OIDC_ISSUER);
    assert_eq!(verified.trust_root, TrustRootSource::Embedded);
}

#[test]
fn real_nightly_bundle_verifies_against_the_embedded_root() {
    // Nightlies are dispatched as `release.yml` at the nightly tag, so they
    // carry the same identity shape; no wider matcher is needed for them.
    let verified = verify_release_bundle(
        &embedded(),
        NIGHTLY_TAG,
        ASSET,
        NIGHTLY_SHA256,
        NIGHTLY_BUNDLE,
    )
    .expect("a published nightly bundle must verify");
    assert_eq!(verified.identity, expected_release_identity(NIGHTLY_TAG));
}

#[test]
fn real_bundle_rejects_tampered_bytes() {
    let err = verify_release_bundle(
        &embedded(),
        BETA_TAG,
        ASSET,
        &tampered(BETA_SHA256),
        BETA_BUNDLE,
    )
    .unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::VerificationFailed { .. }),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(message.contains(BETA_TAG), "{message}");
    assert!(message.contains(ASSET), "{message}");
    assert!(
        message.contains(&expected_release_identity(BETA_TAG)),
        "{message}"
    );
}

#[test]
fn real_bundle_rejects_another_ref_of_the_same_workflow() {
    // A genuine signature from the release workflow, but for beta.62, offered
    // as beta.61: a replayed release must not pass as a different tag.
    let err = verify_release_bundle(
        &embedded(),
        "v0.1.0-beta.61",
        ASSET,
        BETA_SHA256,
        BETA_BUNDLE,
    )
    .unwrap_err();
    match err {
        ReleaseSignatureError::IdentityMismatch {
            tag,
            asset,
            expected,
            observed,
        } => {
            assert_eq!(tag, "v0.1.0-beta.61");
            assert_eq!(asset, ASSET);
            assert_eq!(expected, expected_release_identity("v0.1.0-beta.61"));
            assert_eq!(observed, expected_release_identity(BETA_TAG));
        }
        other => panic!("expected an identity mismatch, got {other:?}"),
    }
}

#[test]
fn real_bundle_from_another_release_is_rejected() {
    // The nightly's bundle offered for the beta tarball: wrong bytes.
    let err = verify_release_bundle(&embedded(), BETA_TAG, ASSET, BETA_SHA256, NIGHTLY_BUNDLE)
        .unwrap_err();
    assert!(
        matches!(
            err,
            ReleaseSignatureError::IdentityMismatch { .. }
                | ReleaseSignatureError::VerificationFailed { .. }
        ),
        "{err:?}"
    );
}

#[test]
fn malformed_bundle_and_digest_are_typed_errors() {
    let err =
        verify_release_bundle(&embedded(), BETA_TAG, ASSET, BETA_SHA256, "{not json").unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::BundleMalformed { ref bundle_asset, .. }
            if bundle_asset == "temps-linux-amd64.tar.gz.sigstore.json"),
        "{err:?}"
    );
    let err =
        verify_release_bundle(&embedded(), BETA_TAG, ASSET, "nothex", BETA_BUNDLE).unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::InvalidDigest { .. }),
        "{err:?}"
    );
}

#[test]
fn generated_bundle_from_the_release_identity_passes() {
    let sigstore = TestSigstore::new();
    let trust = ReleaseTrust::from_root(&sigstore.root, TrustRootSource::Test).unwrap();
    let artifact = b"release tarball bytes";
    let bundle = sigstore.sign(
        artifact,
        &expected_release_identity("v1.2.3"),
        GITHUB_ACTIONS_OIDC_ISSUER,
    );
    let verified =
        verify_release_bundle(&trust, "v1.2.3", ASSET, &sha256_hex(artifact), &bundle).unwrap();
    assert_eq!(verified.identity, expected_release_identity("v1.2.3"));
    assert_eq!(verified.trust_root, TrustRootSource::Test);
}

#[test]
fn generated_bundle_rejects_tampered_bytes() {
    let sigstore = TestSigstore::new();
    let trust = ReleaseTrust::from_root(&sigstore.root, TrustRootSource::Test).unwrap();
    let bundle = sigstore.sign(
        b"release tarball bytes",
        &expected_release_identity("v1.2.3"),
        GITHUB_ACTIONS_OIDC_ISSUER,
    );
    let err = verify_release_bundle(
        &trust,
        "v1.2.3",
        ASSET,
        &sha256_hex(b"release tarball bytes, swapped"),
        &bundle,
    )
    .unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::VerificationFailed { .. }),
        "{err:?}"
    );
}

#[test]
fn valid_signature_from_any_other_identity_is_rejected() {
    let sigstore = TestSigstore::new();
    let trust = ReleaseTrust::from_root(&sigstore.root, TrustRootSource::Test).unwrap();
    let artifact = b"release tarball bytes";
    // Each is a cryptographically valid, logged signature; only who signed differs.
    let impostors = [
        // A fork running the same workflow at the same tag.
        "https://github.com/someone-else/temps/.github/workflows/release.yml@refs/tags/v1.2.3",
        // Another workflow of this repository.
        "https://github.com/gotempsh/temps/.github/workflows/nightly-release.yml@refs/tags/v1.2.3",
        // The release workflow, run from a branch instead of the tag.
        "https://github.com/gotempsh/temps/.github/workflows/release.yml@refs/heads/main",
        // The release workflow at a different tag.
        "https://github.com/gotempsh/temps/.github/workflows/release.yml@refs/tags/v1.2.4",
        // A prefix/suffix of the real identity.
        "https://github.com/gotempsh/temps/.github/workflows/release.yml@refs/tags/v1.2.3-evil",
    ];
    for identity in impostors {
        let bundle = sigstore.sign(artifact, identity, GITHUB_ACTIONS_OIDC_ISSUER);
        let err = verify_release_bundle(&trust, "v1.2.3", ASSET, &sha256_hex(artifact), &bundle)
            .unwrap_err();
        match err {
            ReleaseSignatureError::IdentityMismatch {
                expected, observed, ..
            } => {
                assert_eq!(expected, expected_release_identity("v1.2.3"));
                assert_eq!(observed, identity);
            }
            other => panic!("{identity}: expected an identity mismatch, got {other:?}"),
        }
    }
}

#[test]
fn right_identity_from_another_issuer_is_rejected() {
    let sigstore = TestSigstore::new();
    let trust = ReleaseTrust::from_root(&sigstore.root, TrustRootSource::Test).unwrap();
    let artifact = b"release tarball bytes";
    let bundle = sigstore.sign(
        artifact,
        &expected_release_identity("v1.2.3"),
        "https://issuer.example.invalid",
    );
    let err =
        verify_release_bundle(&trust, "v1.2.3", ASSET, &sha256_hex(artifact), &bundle).unwrap_err();
    match err {
        ReleaseSignatureError::IssuerMismatch {
            expected_issuer,
            observed,
            ..
        } => {
            assert_eq!(expected_issuer, GITHUB_ACTIONS_OIDC_ISSUER);
            assert_eq!(observed, "https://issuer.example.invalid");
        }
        other => panic!("expected an issuer mismatch, got {other:?}"),
    }
}

#[test]
fn bundle_from_an_untrusted_sigstore_instance_is_rejected() {
    // Right identity, right issuer, valid signature, but the CA and logs are
    // not in the trusted root: nobody may run their own "Sigstore".
    let trusted = TestSigstore::new();
    let rogue = TestSigstore::new();
    let trust = ReleaseTrust::from_root(&trusted.root, TrustRootSource::Test).unwrap();
    let artifact = b"release tarball bytes";
    let bundle = rogue.sign(
        artifact,
        &expected_release_identity("v1.2.3"),
        GITHUB_ACTIONS_OIDC_ISSUER,
    );
    let err =
        verify_release_bundle(&trust, "v1.2.3", ASSET, &sha256_hex(artifact), &bundle).unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::VerificationFailed { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn missing_bundle_is_refused_without_the_flag() {
    let release = test_release("v0.0.8", &[(ASSET, "https://example.invalid/a".into())]);
    let err = verify_release_asset(
        &ReleaseTrustSource::PublicGood,
        &release,
        ASSET,
        BETA_SHA256,
        false,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::BundleMissing { .. }),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(message.contains("v0.0.8"), "{message}");
    assert!(
        message.contains("temps-linux-amd64.tar.gz.sigstore.json"),
        "{message}"
    );
    assert!(message.contains(ALLOW_UNSIGNED_FLAG), "{message}");
}

#[tokio::test]
async fn missing_bundle_is_accepted_with_the_flag() {
    let release = test_release("v0.0.8", &[(ASSET, "https://example.invalid/a".into())]);
    let check = verify_release_asset(
        &ReleaseTrustSource::PublicGood,
        &release,
        ASSET,
        BETA_SHA256,
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        check,
        SignatureCheck::UnsignedAllowed {
            bundle_asset: "temps-linux-amd64.tar.gz.sigstore.json".to_string()
        }
    );
    assert!(check.summary().contains(ALLOW_UNSIGNED_FLAG));
}

/// Serve one bundle body from localhost.
async fn serve_bundle(body: String) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/bundle", listener.local_addr().unwrap());
    let app = axum::Router::new().route("/bundle", axum::routing::get(move || async { body }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, server)
}

#[tokio::test]
async fn published_bundle_is_downloaded_and_verified() {
    let sigstore = TestSigstore::new();
    let artifact = b"release tarball bytes";
    let bundle = sigstore.sign(
        artifact,
        &expected_release_identity("v1.2.3"),
        GITHUB_ACTIONS_OIDC_ISSUER,
    );
    let (url, server) = serve_bundle(bundle).await;
    let release = test_release(
        "v1.2.3",
        &[
            (ASSET, "https://example.invalid/a".into()),
            ("temps-linux-amd64.tar.gz.sigstore.json", url),
        ],
    );
    let check = verify_release_asset(
        &sigstore.trust(),
        &release,
        ASSET,
        &sha256_hex(artifact),
        false,
    )
    .await;
    server.abort();
    match check.unwrap() {
        SignatureCheck::Verified(signature) => {
            assert_eq!(signature.identity, expected_release_identity("v1.2.3"));
        }
        other => panic!("expected a verified signature, got {other:?}"),
    }
}

#[tokio::test]
async fn allow_unsigned_never_excuses_a_bad_signature() {
    let sigstore = TestSigstore::new();
    let bundle = sigstore.sign(
        b"what was signed",
        &expected_release_identity("v1.2.3"),
        GITHUB_ACTIONS_OIDC_ISSUER,
    );
    let (url, server) = serve_bundle(bundle).await;
    let release = test_release(
        "v1.2.3",
        &[
            (ASSET, "https://example.invalid/a".into()),
            ("temps-linux-amd64.tar.gz.sigstore.json", url),
        ],
    );
    let err = verify_release_asset(
        &sigstore.trust(),
        &release,
        ASSET,
        &sha256_hex(b"what was downloaded"),
        true,
    )
    .await
    .unwrap_err();
    server.abort();
    assert!(
        matches!(err, ReleaseSignatureError::VerificationFailed { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn listed_but_unreachable_bundle_fails_closed() {
    let release = test_release(
        "v1.2.3",
        &[
            (ASSET, "https://example.invalid/a".into()),
            (
                "temps-linux-amd64.tar.gz.sigstore.json",
                // Nothing listens on port 9 of localhost.
                "http://127.0.0.1:9/bundle".into(),
            ),
        ],
    );
    let err = verify_release_asset(
        &ReleaseTrustSource::PublicGood,
        &release,
        ASSET,
        BETA_SHA256,
        true,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::BundleDownload { .. }),
        "{err:?}"
    );
}

#[test]
fn plan_says_whether_a_release_is_signed() {
    let signed = test_release(
        "v1.2.3",
        &[
            (ASSET, "https://example.invalid/a".into()),
            (
                "temps-linux-amd64.tar.gz.sigstore.json",
                "https://example.invalid/b".into(),
            ),
        ],
    );
    assert!(
        describe_release_signature(&signed, ASSET).contains(&expected_release_identity("v1.2.3"))
    );
    let unsigned = test_release("v0.0.8", &[(ASSET, "https://example.invalid/a".into())]);
    assert!(describe_release_signature(&unsigned, ASSET).contains(ALLOW_UNSIGNED_FLAG));
}

#[test]
fn unsigned_release_is_refused_before_the_download() {
    let unsigned = test_release("v0.0.8", &[(ASSET, "https://example.invalid/a".into())]);
    let err = ensure_signature_published(&unsigned, ASSET, false).unwrap_err();
    assert!(
        matches!(err, ReleaseSignatureError::BundleMissing { ref tag, .. } if tag == "v0.0.8"),
        "{err:?}"
    );
    ensure_signature_published(&unsigned, ASSET, true).unwrap();
    let signed = test_release(
        "v1.2.3",
        &[
            (ASSET, "https://example.invalid/a".into()),
            (
                "temps-linux-amd64.tar.gz.sigstore.json",
                "https://example.invalid/b".into(),
            ),
        ],
    );
    ensure_signature_published(&signed, ASSET, false).unwrap();
}
