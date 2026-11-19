// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sigstore verification of a release tarball before it is installed.
//!
//! A `.sha256` asset proves the download is intact, not who published it:
//! whoever can replace `temps-<platform>.tar.gz` on a release can replace its
//! checksum too. The release workflow therefore signs every tarball keylessly
//! with cosign and publishes the result as `<tarball>.sigstore.json`. This
//! module checks that bundle in-process (servers do not have cosign) before
//! `temps upgrade`, the console's self-update or the guest-agent fetch unpack
//! anything.
//!
//! What is pinned: the certificate must be issued by Sigstore's public-good
//! Fulcio to `https://github.com/gotempsh/temps/.github/workflows/release.yml`
//! at `refs/tags/<the tag being installed>`, through GitHub Actions OIDC. The
//! match is exact. Nightly releases are produced by `nightly-release.yml`
//! dispatching `release.yml` at the new tag (not calling it as a reusable
//! workflow), so stable, beta and nightly releases all carry this same
//! identity; anything broader would also accept other workflows or other refs
//! of this repository.
//!
//! Trust root: Sigstore's trusted root (Fulcio CAs, Rekor and CT log keys,
//! timestamp authorities) is refreshed through Sigstore's TUF repository,
//! bootstrapped from the TUF root embedded in `sigstore-trust-root`, with a
//! bounded deadline and no on-disk cache. When the refresh fails (no egress to
//! `tuf-repo-cdn.sigstore.dev`, a proxy, a timeout) the trusted root embedded
//! in the binary is used instead, and that is logged. Either way the signature
//! itself is checked offline and fails closed.

use std::fmt;
use std::time::Duration;

use sigstore_verify::trust_root::{TrustedRoot, TufConfig, SIGSTORE_PRODUCTION_TRUSTED_ROOT};
use sigstore_verify::types::{Bundle, Sha256Hash};
use sigstore_verify::{IdentityMatcher, VerificationPolicy, Verifier};
use tracing::{info, warn};

use super::GitHubRelease;

/// The repository whose release workflow signs every release tarball.
const RELEASE_REPOSITORY_URL: &str = "https://github.com/gotempsh/temps";

/// The workflow, relative to the repository, that signs release tarballs.
const RELEASE_WORKFLOW_PATH: &str = ".github/workflows/release.yml";

/// The OIDC issuer of GitHub Actions workload identities.
pub(crate) const GITHUB_ACTIONS_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// Suffix of the Sigstore bundle published beside each signed asset.
pub(crate) const SIGSTORE_BUNDLE_SUFFIX: &str = ".sigstore.json";

/// The CLI flag that accepts a release without a Sigstore bundle.
pub(crate) const ALLOW_UNSIGNED_FLAG: &str = "--allow-unsigned";

/// Deadline for refreshing the trusted root through TUF. Past it, the
/// embedded root is used; an upgrade is never blocked on Sigstore's CDN.
const TUF_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Deadline for downloading a bundle (about 10 KB in practice).
const BUNDLE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// A bundle larger than this is refused rather than buffered.
const MAX_BUNDLE_BYTES: usize = 1024 * 1024;

/// Attempts at downloading a bundle. A connection error, a timeout, HTTP 408,
/// 429 or 5xx is retried with backoff; anything else fails at once.
const BUNDLE_DOWNLOAD_ATTEMPTS: u32 = 3;

/// First backoff delay between bundle download attempts (then doubled).
#[cfg(not(test))]
const BUNDLE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
#[cfg(test)]
const BUNDLE_RETRY_BASE_DELAY: Duration = Duration::from_millis(10);

/// The certificate identity a genuine signature on `tag` carries.
pub(crate) fn expected_release_identity(tag: &str) -> String {
    format!("{RELEASE_REPOSITORY_URL}/{RELEASE_WORKFLOW_PATH}@refs/tags/{tag}")
}

/// The name of the bundle asset that signs `asset_name`.
pub(crate) fn bundle_asset_name(asset_name: &str) -> String {
    format!("{asset_name}{SIGSTORE_BUNDLE_SUFFIX}")
}

/// Why a release asset's signature could not be accepted.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReleaseSignatureError {
    #[error(
        "Release {tag} publishes no Sigstore signature for {asset} (no '{bundle_asset}' asset), \
         so its origin cannot be verified. Refusing to install an unsigned release. Releases \
         published before signing was introduced have none; to install one anyway on the \
         strength of its SHA-256 checksum alone, run `temps upgrade --version {tag} \
         {ALLOW_UNSIGNED_FLAG}`"
    )]
    BundleMissing {
        tag: String,
        asset: String,
        bundle_asset: String,
    },

    #[error(
        "Failed to download the Sigstore bundle '{bundle_asset}' of release {tag}: {reason}. \
         Refusing to install {asset} without verifying its signature"
    )]
    BundleDownload {
        tag: String,
        asset: String,
        bundle_asset: String,
        reason: String,
    },

    #[error(
        "The Sigstore bundle '{bundle_asset}' of release {tag} is not a valid bundle: {reason}. \
         Refusing to install {asset}"
    )]
    BundleMalformed {
        tag: String,
        asset: String,
        bundle_asset: String,
        reason: String,
    },

    #[error(
        "The downloaded {asset} of release {tag} has an unusable SHA-256 digest '{digest}': \
         {reason}"
    )]
    InvalidDigest {
        tag: String,
        asset: String,
        digest: String,
        reason: String,
    },

    #[error(
        "Could not load the Sigstore trusted root ({trust_root}) to verify {asset} of release \
         {tag}: {reason}. Refusing to install it unverified"
    )]
    TrustRoot {
        tag: String,
        asset: String,
        trust_root: TrustRootSource,
        reason: String,
    },

    #[error(
        "The Sigstore signature on {asset} of release {tag} was made by '{observed}', not by the \
         Temps release workflow '{expected}'. Refusing to install it"
    )]
    IdentityMismatch {
        tag: String,
        asset: String,
        expected: String,
        observed: String,
    },

    #[error(
        "The Sigstore signature on {asset} of release {tag} names OIDC issuer '{observed}', \
         expected '{expected_issuer}' for signer '{expected}'. Refusing to install it"
    )]
    IssuerMismatch {
        tag: String,
        asset: String,
        expected: String,
        expected_issuer: String,
        observed: String,
    },

    #[error(
        "Sigstore verification of {asset} from release {tag} failed: {reason} (expected signer \
         '{expected}', issuer '{issuer}'). The download or its signature may have been tampered \
         with; refusing to install it"
    )]
    VerificationFailed {
        tag: String,
        asset: String,
        expected: String,
        issuer: String,
        reason: String,
    },
}

/// Which trusted root a verification ran against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustRootSource {
    /// Refreshed from Sigstore's TUF repository during this run.
    Tuf,
    /// Compiled into this binary (`sigstore-trust-root`).
    Embedded,
    /// Supplied by a test.
    #[cfg(test)]
    Test,
}

impl fmt::Display for TrustRootSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tuf => "Sigstore TUF repository",
            Self::Embedded => "embedded Sigstore trusted root",
            #[cfg(test)]
            Self::Test => "test trusted root",
        })
    }
}

/// Where the trusted root comes from. Production always uses
/// [`ReleaseTrustSource::PublicGood`]; tests pin their own instance.
#[derive(Clone)]
pub(crate) enum ReleaseTrustSource {
    /// Sigstore's public-good instance: TUF refresh, embedded fallback.
    PublicGood,
    #[cfg(test)]
    Fixed(std::sync::Arc<TrustedRoot>),
}

/// A trusted root ready to verify bundles.
pub(crate) struct ReleaseTrust {
    verifier: Verifier,
    source: TrustRootSource,
}

impl ReleaseTrust {
    fn from_root(
        root: &TrustedRoot,
        source: TrustRootSource,
    ) -> Result<Self, sigstore_verify::Error> {
        Ok(Self {
            verifier: Verifier::new(root)?,
            source,
        })
    }

    /// The root compiled into this binary.
    fn embedded() -> Result<Self, String> {
        let root =
            TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT).map_err(|e| e.to_string())?;
        Self::from_root(&root, TrustRootSource::Embedded).map_err(|e| e.to_string())
    }

    /// Refresh through TUF, falling back to the embedded root.
    async fn public_good() -> Result<Self, String> {
        let refreshed = tokio::time::timeout(
            TUF_REFRESH_TIMEOUT,
            TrustedRoot::from_tuf(TufConfig::production().without_cache()),
        )
        .await;
        let reason = match refreshed {
            Ok(Ok(root)) => match Self::from_root(&root, TrustRootSource::Tuf) {
                Ok(trust) => return Ok(trust),
                Err(e) => format!("the refreshed trusted root is unusable: {e}"),
            },
            Ok(Err(e)) => e.to_string(),
            Err(_) => format!("timed out after {}s", TUF_REFRESH_TIMEOUT.as_secs()),
        };
        warn!(
            reason = %reason,
            "Could not refresh the Sigstore trusted root through TUF; verifying against the \
             trusted root embedded in this binary"
        );
        Self::embedded()
    }
}

impl ReleaseTrustSource {
    async fn load(&self) -> Result<ReleaseTrust, (TrustRootSource, String)> {
        match self {
            Self::PublicGood => ReleaseTrust::public_good()
                .await
                .map_err(|e| (TrustRootSource::Embedded, e)),
            #[cfg(test)]
            Self::Fixed(root) => ReleaseTrust::from_root(root, TrustRootSource::Test)
                .map_err(|e| (TrustRootSource::Test, e.to_string())),
        }
    }
}

/// A signature that verified, and what it was checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedSignature {
    pub(crate) identity: String,
    pub(crate) issuer: String,
    pub(crate) trust_root: TrustRootSource,
}

/// The outcome of checking a release asset's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignatureCheck {
    Verified(VerifiedSignature),
    /// The release has no bundle and the operator passed `--allow-unsigned`.
    UnsignedAllowed {
        bundle_asset: String,
    },
}

impl SignatureCheck {
    /// One line for the operator, printed after the checksum line.
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Verified(signature) => format!(
                "Signature verified (Sigstore, signed by {})",
                signature.identity
            ),
            Self::UnsignedAllowed { bundle_asset } => format!(
                "WARNING: no signature ({bundle_asset} is not published); installing on the \
                 SHA-256 checksum alone because {ALLOW_UNSIGNED_FLAG} was passed"
            ),
        }
    }
}

/// Refuse an unsigned release before spending a download on it. The same
/// rule is enforced again by [`verify_release_asset`] after the download.
pub(crate) fn ensure_signature_published(
    release: &GitHubRelease,
    asset: &str,
    allow_unsigned: bool,
) -> Result<(), ReleaseSignatureError> {
    let bundle_asset = bundle_asset_name(asset);
    if allow_unsigned || release.assets.iter().any(|a| a.name == bundle_asset) {
        return Ok(());
    }
    Err(ReleaseSignatureError::BundleMissing {
        tag: release.tag_name.clone(),
        asset: asset.to_string(),
        bundle_asset,
    })
}

/// What the upgrade plan says about `asset`'s signature, before downloading.
pub(crate) fn describe_release_signature(release: &GitHubRelease, asset: &str) -> String {
    let bundle_asset = bundle_asset_name(asset);
    if release.assets.iter().any(|a| a.name == bundle_asset) {
        format!(
            "Sigstore bundle published; will be verified against {}",
            expected_release_identity(&release.tag_name)
        )
    } else {
        format!(
            "none published ({bundle_asset} is missing); installing requires {ALLOW_UNSIGNED_FLAG}"
        )
    }
}

/// Verify `bundle_json` over the asset whose SHA-256 is `sha256_hex`.
///
/// Pure and synchronous: everything it trusts is in `trust`.
pub(crate) fn verify_release_bundle(
    trust: &ReleaseTrust,
    tag: &str,
    asset: &str,
    sha256_hex: &str,
    bundle_json: &str,
) -> Result<VerifiedSignature, ReleaseSignatureError> {
    let expected = expected_release_identity(tag);
    let bundle =
        Bundle::from_json(bundle_json).map_err(|e| ReleaseSignatureError::BundleMalformed {
            tag: tag.to_string(),
            asset: asset.to_string(),
            bundle_asset: bundle_asset_name(asset),
            reason: e.to_string(),
        })?;
    let digest =
        Sha256Hash::from_hex(sha256_hex).map_err(|e| ReleaseSignatureError::InvalidDigest {
            tag: tag.to_string(),
            asset: asset.to_string(),
            digest: sha256_hex.to_string(),
            reason: e.to_string(),
        })?;
    // URI-typed: an email SAN that happens to spell the URL does not match.
    let policy = VerificationPolicy::new(
        IdentityMatcher::Uri(expected.clone()),
        GITHUB_ACTIONS_OIDC_ISSUER,
    );

    let result = trust
        .verifier
        .verify(digest, &bundle, &policy)
        .map_err(|e| match e {
            sigstore_verify::Error::IdentityMismatch { actual, .. } => {
                ReleaseSignatureError::IdentityMismatch {
                    tag: tag.to_string(),
                    asset: asset.to_string(),
                    expected: expected.clone(),
                    observed: actual
                        .map(|identity| identity.as_str().to_string())
                        .unwrap_or_else(|| "<certificate has no identity>".to_string()),
                }
            }
            sigstore_verify::Error::IssuerMismatch { actual, .. } => {
                ReleaseSignatureError::IssuerMismatch {
                    tag: tag.to_string(),
                    asset: asset.to_string(),
                    expected: expected.clone(),
                    expected_issuer: GITHUB_ACTIONS_OIDC_ISSUER.to_string(),
                    observed: actual.unwrap_or_else(|| "<certificate has no issuer>".to_string()),
                }
            }
            other => ReleaseSignatureError::VerificationFailed {
                tag: tag.to_string(),
                asset: asset.to_string(),
                expected: expected.clone(),
                issuer: GITHUB_ACTIONS_OIDC_ISSUER.to_string(),
                reason: other.to_string(),
            },
        })?;

    // The policy already enforced both; refuse to report a pass that did not
    // check the full chain, the CT log and the transparency log.
    if !(result.certificate_verified()
        && result.sct_verified()
        && result.tlog_verified()
        && result.identity_policy_checked())
    {
        return Err(ReleaseSignatureError::VerificationFailed {
            tag: tag.to_string(),
            asset: asset.to_string(),
            expected,
            issuer: GITHUB_ACTIONS_OIDC_ISSUER.to_string(),
            reason: "the verifier skipped a required check (certificate chain, SCT, \
                     transparency log or identity policy)"
                .to_string(),
        });
    }

    let identity = result
        .identity()
        .map(|identity| identity.as_str().to_string())
        .unwrap_or(expected);
    let issuer = result
        .issuer()
        .unwrap_or(GITHUB_ACTIONS_OIDC_ISSUER)
        .to_string();
    info!(
        tag = %tag,
        asset = %asset,
        identity = %identity,
        issuer = %issuer,
        trust_root = %trust.source,
        "Verified the Sigstore signature of a release asset"
    );
    Ok(VerifiedSignature {
        identity,
        issuer,
        trust_root: trust.source,
    })
}

/// Download `release`'s bundle for `asset` and verify the asset against it.
///
/// `sha256_hex` is the digest computed while the asset streamed to disk, so
/// the bytes that were checked are the bytes that get unpacked. A release
/// without a bundle is refused unless `allow_unsigned`; a bundle that exists
/// but does not verify is always refused.
pub(crate) async fn verify_release_asset(
    trust: &ReleaseTrustSource,
    release: &GitHubRelease,
    asset: &str,
    sha256_hex: &str,
    allow_unsigned: bool,
) -> Result<SignatureCheck, ReleaseSignatureError> {
    let tag = release.tag_name.as_str();
    let bundle_asset = bundle_asset_name(asset);
    let Some(bundle) = release.assets.iter().find(|a| a.name == bundle_asset) else {
        if allow_unsigned {
            warn!(
                tag = %tag,
                asset = %asset,
                bundle_asset = %bundle_asset,
                "Installing a release with no Sigstore signature because {ALLOW_UNSIGNED_FLAG} \
                 was passed; only its SHA-256 checksum was verified"
            );
            return Ok(SignatureCheck::UnsignedAllowed { bundle_asset });
        }
        return Err(ReleaseSignatureError::BundleMissing {
            tag: tag.to_string(),
            asset: asset.to_string(),
            bundle_asset,
        });
    };

    let bundle_json = download_bundle(&bundle.browser_download_url)
        .await
        .map_err(|reason| ReleaseSignatureError::BundleDownload {
            tag: tag.to_string(),
            asset: asset.to_string(),
            bundle_asset: bundle_asset.clone(),
            reason,
        })?;
    let trust =
        trust
            .load()
            .await
            .map_err(|(trust_root, reason)| ReleaseSignatureError::TrustRoot {
                tag: tag.to_string(),
                asset: asset.to_string(),
                trust_root,
                reason,
            })?;
    verify_release_bundle(&trust, tag, asset, sha256_hex, &bundle_json)
        .map(SignatureCheck::Verified)
}

/// Why one bundle download attempt failed, and whether another may succeed.
#[derive(Debug)]
struct BundleFetchFailure {
    reason: String,
    transient: bool,
}

impl BundleFetchFailure {
    fn transient(reason: String) -> Self {
        Self {
            reason,
            transient: true,
        }
    }

    fn permanent(reason: String) -> Self {
        Self {
            reason,
            transient: false,
        }
    }
}

impl fmt::Display for BundleFetchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Fetch a bundle body, bounded in time and size, retrying transient
/// failures (connection errors, timeouts, HTTP 408/429/5xx) with backoff.
async fn download_bundle(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(BUNDLE_DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))?;
    temps_core::retry::RetryConfig::new(BUNDLE_DOWNLOAD_ATTEMPTS)
        .with_base_delay(BUNDLE_RETRY_BASE_DELAY)
        .retry_if(
            || fetch_bundle_once(&client, url),
            |failure: &BundleFetchFailure| failure.transient,
        )
        .await
        .map_err(|failure| {
            if failure.transient {
                format!(
                    "{} (after {BUNDLE_DOWNLOAD_ATTEMPTS} attempts)",
                    failure.reason
                )
            } else {
                failure.reason
            }
        })
}

async fn fetch_bundle_once(
    client: &reqwest::Client,
    url: &str,
) -> Result<String, BundleFetchFailure> {
    let mut response = client
        .get(url)
        .header("User-Agent", "temps-self-upgrade")
        .send()
        .await
        .map_err(|e| BundleFetchFailure::transient(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let reason = format!("HTTP {status}");
        let transient = status.is_server_error()
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        return Err(BundleFetchFailure { reason, transient });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| BundleFetchFailure::transient(e.to_string()))?
    {
        if body.len() + chunk.len() > MAX_BUNDLE_BYTES {
            return Err(BundleFetchFailure::permanent(format!(
                "bundle is larger than {MAX_BUNDLE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body)
        .map_err(|e| BundleFetchFailure::permanent(format!("bundle is not UTF-8: {e}")))
}

#[cfg(test)]
pub(crate) mod test_sigstore;
#[cfg(test)]
mod tests;
