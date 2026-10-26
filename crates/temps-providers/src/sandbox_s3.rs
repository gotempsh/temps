// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox access to a linked S3-compatible service (ADR-051).
//!
//! A deployment connects with the service's root key to a bucket named for its
//! environment. A sandbox runs code nobody reviewed, so it gets a bucket of its
//! own (`<project>.sandbox`, or `<sandbox>.id.sandbox` for a sandbox linked
//! directly; see [`sandbox_bucket_name`]) and a *temporary*
//! credential that can use that
//! bucket and nothing else: the service's own STS `AssumeRole` is asked for a
//! key restricted by a session policy to one bucket. Nothing is stored. The key
//! expires by itself, and every command a sandbox runs is issued a fresh one,
//! so a leaked value stops working within [`CREDENTIAL_LIFETIME_SECS`].
//!
//! The service's root key never leaves the control plane. Talking to the
//! service takes SigV4, which is small enough to do here with the HMAC the
//! crate already depends on, rather than pulling in an STS client for a single
//! call.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use temps_entities::external_services;
use tracing::info;

use crate::externalsvc::managed_s3::{ManagedS3BackendKind, ManagedS3BackendSelection};
use crate::externalsvc::rustfs::{rustfs_container_name, DEFAULT_RUSTFS_API_PORT};
use crate::sandbox_engines::{SandboxAccessEngine, SandboxAccessRequest, SandboxNamespace};
use crate::services::{ExternalServiceError, ExternalServiceManager};

/// How long a temporary credential lasts. Long enough that a dev server
/// started in the sandbox keeps working through a working day, short enough
/// that a leaked key is not a standing one.
const CREDENTIAL_LIFETIME_SECS: u32 = 12 * 60 * 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_REGION: &str = "us-east-1";
/// S3 bucket names: 3 to 63 characters.
const BUCKET_NAME_MAX: usize = 63;
/// A `.` never appears in a deployment's bucket name (scopes are reduced to
/// `[a-z0-9-]` and declared bucket names refuse dots), so no environment,
/// project or declared bucket can be named like a sandbox's.
const BUCKET_SUFFIX: &str = ".sandbox";

type HmacSha256 = Hmac<Sha256>;

/// Why a request to the service could not be built or signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxS3Error {
    /// The bucket name derived from the project slug is not a valid S3 name.
    UnsafeBucketName { value: String },
    /// The service's root secret could not key the signature.
    Signing { reason: String },
    /// The service's STS answer carried no usable credential.
    MalformedCredentials { reason: String },
}

impl std::fmt::Display for SandboxS3Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeBucketName { value } => write!(
                formatter,
                "the bucket name '{value}' is not a valid S3 bucket name (3 to {BUCKET_NAME_MAX} \
                 lowercase letters, digits and hyphens, not starting or ending with a hyphen)"
            ),
            Self::Signing { reason } => {
                write!(formatter, "could not sign the request: {reason}")
            }
            Self::MalformedCredentials { reason } => {
                write!(
                    formatter,
                    "the service's STS answer carried no usable credential: {reason}"
                )
            }
        }
    }
}

/// The bucket a namespace's sandboxes share on the service.
///
/// | namespace                     | bucket                    |
/// |-------------------------------|---------------------------|
/// | project, plain slug           | `<slug>.sandbox`          |
/// | project, any other slug       | `<head>-<digest>.h.sandbox` |
/// | sandbox linked directly       | `<id>.id.sandbox`         |
/// | sandbox, id not plain         | `<head>-<digest>.idh.sandbox` |
///
/// The first label never contains a dot, so the labels after it say which
/// row a name came from and no two rows can produce the same bucket: a
/// project slugged like a sandbox's id gets `<id>.sandbox`, never that
/// sandbox's `<id>.id.sandbox`. Within a row a plain value is used as it is
/// (one-to-one), and anything else (an accented, long or odd slug) is hashed
/// whole with 96 bits of SHA-256.
///
/// Sandboxes used to get `<project>-sandbox`, which is exactly the bucket a
/// deployment gives an environment slugged `sandbox` (or a project slugged
/// `<project>-sandbox`), so a sandbox could be handed a deployment's bucket.
/// That old name is not reused: nothing only the root key can set marks such
/// a bucket as a sandbox's, so a sandbox's old bucket and an environment's
/// cannot be told apart. Its objects stay where they are; new sandbox data
/// goes to the new bucket.
pub fn sandbox_bucket_name(namespace: &SandboxNamespace) -> Result<String, SandboxS3Error> {
    let (direct_marker, hashed_marker) = match namespace {
        SandboxNamespace::Project(_) => ("", ".h"),
        SandboxNamespace::Sandbox(_) => (".id", ".idh"),
    };
    let (direct, whole) = namespace.parts();
    let direct_room = BUCKET_NAME_MAX - direct_marker.len() - BUCKET_SUFFIX.len();
    let name = match direct.filter(|value| is_plain_bucket_label(value, direct_room)) {
        Some(label) => format!("{label}{direct_marker}{BUCKET_SUFFIX}"),
        None => {
            let room = BUCKET_NAME_MAX - hashed_marker.len() - BUCKET_SUFFIX.len();
            format!(
                "{}{hashed_marker}{BUCKET_SUFFIX}",
                hashed_bucket_label(whole, room)
            )
        }
    };
    // Every row above is valid by construction; this is the backstop.
    if is_valid_sandbox_bucket(&name) {
        Ok(name)
    } else {
        Err(SandboxS3Error::UnsafeBucketName { value: name })
    }
}

/// A label used as it is: `[a-z0-9-]`, starting and ending with a letter or
/// digit, no `--` (which also rules out the reserved `xn--`), not the
/// reserved `sthree-`, at most `max` bytes.
fn is_plain_bucket_label(value: &str, max: usize) -> bool {
    crate::sandbox_engines::is_plain_slug(value)
        && value.len() <= max
        && !value.ends_with('-')
        && !value.contains("--")
        && !value.starts_with("sthree-")
}

/// `<head>-<digest>` (or `<digest>` with no usable head): a readable head
/// from `value`'s letters and digits, and 96 bits of SHA-256 of all of it.
fn hashed_bucket_label(value: &str, max: usize) -> String {
    let digest = hex::encode(Sha256::digest(value.as_bytes()));
    let digest = &digest[..crate::sandbox_engines::NAME_DIGEST_LEN];
    let mut head = String::new();
    for character in value
        .chars()
        .map(|character| character.to_ascii_lowercase())
    {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            head.push(character);
        } else if !head.is_empty() && !head.ends_with('-') {
            head.push('-');
        }
    }
    head.truncate(max.saturating_sub(digest.len() + 1));
    let head = head.trim_end_matches('-');
    if head.is_empty() || head.starts_with("sthree") {
        digest.to_string()
    } else {
        format!("{head}-{digest}")
    }
}

/// 3 to 63 bytes of `[a-z0-9.-]`, labels starting and ending with a letter or
/// digit (so no `..`, `.-` or `-.`).
fn is_valid_sandbox_bucket(name: &str) -> bool {
    (3..=BUCKET_NAME_MAX).contains(&name.len())
        && name.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// The session policy: the object and listing operations of one bucket, and
/// nothing on any other. It intersects with whatever the root key may do, so
/// it can only narrow it. Bucket-level administration (deleting the bucket,
/// changing its policy) is left out on purpose.
pub fn session_policy(bucket: &str) -> serde_json::Value {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "s3:GetBucketLocation",
                "s3:ListBucket",
                "s3:ListBucketMultipartUploads",
                "s3:GetObject",
                "s3:PutObject",
                "s3:DeleteObject",
                "s3:AbortMultipartUpload",
                "s3:ListMultipartUploadParts",
                "s3:GetObjectTagging",
                "s3:PutObjectTagging",
                "s3:DeleteObjectTagging",
            ],
            "Resource": [
                format!("arn:aws:s3:::{bucket}"),
                format!("arn:aws:s3:::{bucket}/*"),
            ],
        }],
    })
}

/// A temporary credential from the service's STS. Secret: never log it.
#[derive(Clone, PartialEq, Eq)]
pub struct TemporaryCredentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: String,
}

impl std::fmt::Debug for TemporaryCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TemporaryCredentials")
            .field("access_key", &"***")
            .field("secret_key", &"***")
            .field("session_token", &"***")
            .finish()
    }
}

/// Read the credential out of an STS `AssumeRole` answer.
pub fn parse_assume_role_response(xml: &str) -> Result<TemporaryCredentials, SandboxS3Error> {
    let field = |tag: &str| {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = xml
            .find(&open)
            .map(|index| index + open.len())
            .ok_or_else(|| SandboxS3Error::MalformedCredentials {
                reason: format!("no <{tag}> in the answer"),
            })?;
        let length =
            xml[start..]
                .find(&close)
                .ok_or_else(|| SandboxS3Error::MalformedCredentials {
                    reason: format!("<{tag}> is not closed"),
                })?;
        let value = xml[start..start + length].trim();
        if value.is_empty() {
            return Err(SandboxS3Error::MalformedCredentials {
                reason: format!("<{tag}> is empty"),
            });
        }
        Ok(value.to_string())
    };
    Ok(TemporaryCredentials {
        access_key: field("AccessKeyId")?,
        secret_key: field("SecretAccessKey")?,
        session_token: field("SessionToken")?,
    })
}

/// Headers that authenticate one request under AWS Signature Version 4.
///
/// `host` is the exact `Host` header the request will carry (with the port
/// when it is not the scheme's default); `query` is empty for every request
/// made here.
#[allow(clippy::too_many_arguments)]
pub fn sign_request(
    method: &str,
    host: &str,
    path: &str,
    content_type: Option<&str>,
    body: &[u8],
    service: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
    now: DateTime<Utc>,
) -> Result<Vec<(String, String)>, SandboxS3Error> {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload_hash = hex::encode(Sha256::digest(body));

    let mut headers = vec![
        ("host".to_string(), host.to_string()),
        ("x-amz-content-sha256".to_string(), payload_hash.clone()),
        ("x-amz-date".to_string(), amz_date.clone()),
    ];
    if let Some(content_type) = content_type {
        headers.push(("content-type".to_string(), content_type.to_string()));
    }
    headers.sort();
    let canonical_headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request =
        format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let key = signing_key(secret_key, &date, region, service)?;
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes())?);

    let mut out = vec![
        ("x-amz-content-sha256".to_string(), payload_hash),
        ("x-amz-date".to_string(), amz_date),
        (
            "authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, \
                 Signature={signature}"
            ),
        ),
    ];
    if let Some(content_type) = content_type {
        out.push(("content-type".to_string(), content_type.to_string()));
    }
    Ok(out)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Result<Vec<u8>, SandboxS3Error> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| SandboxS3Error::Signing {
        reason: error.to_string(),
    })?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// The SigV4 signing key for a day, region and service.
pub fn signing_key(
    secret_key: &str,
    date: &str,
    region: &str,
    service: &str,
) -> Result<Vec<u8>, SandboxS3Error> {
    let date_key = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), date.as_bytes())?;
    let region_key = hmac_sha256(&date_key, region.as_bytes())?;
    let service_key = hmac_sha256(&region_key, service.as_bytes())?;
    hmac_sha256(&service_key, b"aws4_request")
}

/// The variables a sandbox runs with: the names a deployment gets, so an
/// application reads them unchanged, plus the session token a temporary key
/// needs. `container` and `port` are where the sandbox dials the service on
/// its data network.
pub fn environment(
    container: &str,
    port: u16,
    region: &str,
    bucket: &str,
    credentials: &TemporaryCredentials,
) -> HashMap<String, String> {
    let endpoint = format!("http://{container}:{port}");
    HashMap::from([
        ("S3_BUCKET".to_string(), bucket.to_string()),
        ("S3_ENDPOINT".to_string(), endpoint.clone()),
        ("S3_HOST".to_string(), container.to_string()),
        ("S3_PORT".to_string(), port.to_string()),
        ("S3_ACCESS_KEY".to_string(), credentials.access_key.clone()),
        ("S3_SECRET_KEY".to_string(), credentials.secret_key.clone()),
        (
            "S3_SESSION_TOKEN".to_string(),
            credentials.session_token.clone(),
        ),
        ("S3_REGION".to_string(), region.to_string()),
        (
            "AWS_ACCESS_KEY_ID".to_string(),
            credentials.access_key.clone(),
        ),
        (
            "AWS_SECRET_ACCESS_KEY".to_string(),
            credentials.secret_key.clone(),
        ),
        (
            "AWS_SESSION_TOKEN".to_string(),
            credentials.session_token.clone(),
        ),
        ("AWS_DEFAULT_REGION".to_string(), region.to_string()),
        ("AWS_ENDPOINT_URL".to_string(), endpoint),
    ])
}

/// Whether the service is a RustFS-backed one, the only backend whose
/// container and STS this engine knows.
fn is_rustfs_backed(
    service: &external_services::Model,
    parameters: &HashMap<String, serde_json::Value>,
) -> bool {
    let value = match serde_json::to_value(parameters) {
        Ok(value) => value,
        Err(_) => return false,
    };
    // A legacy service still running the MinIO server is named and addressed
    // differently (`minio-<name>`); dialling it as RustFS would reach nothing,
    // so it is left out like any other type without an engine.
    let runs_minio = parameters
        .get("server")
        .and_then(|server| server.as_str())
        .is_some_and(|server| server.trim().eq_ignore_ascii_case("minio"));
    service.node_id.is_none()
        && !runs_minio
        && ManagedS3BackendSelection::from_parameters(&value)
            .map(|selection| selection.backend == ManagedS3BackendKind::Rustfs)
            .unwrap_or(false)
}

/// Sandbox access to a RustFS-backed S3 service: a bucket of the sandbox's own
/// and a bucket-scoped temporary key.
pub struct S3SandboxAccess;

struct ServiceEndpoint {
    /// How the control plane dials the service: its published host port.
    authority: String,
    region: String,
    access_key: String,
    secret_key: String,
}

impl ServiceEndpoint {
    fn from_parameters(
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<Self, ExternalServiceError> {
        let text = |key: &str| {
            parameters.get(key).and_then(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| value.as_u64().map(|number| number.to_string()))
                    .filter(|value| !value.is_empty())
            })
        };
        let required = |key: &str| {
            text(key).ok_or_else(|| ExternalServiceError::ParameterValidationFailed {
                service_id: service.id,
                reason: format!("The S3 service has no '{key}' parameter"),
            })
        };
        let host = text("host").unwrap_or_else(|| "localhost".to_string());
        let port = text("port").unwrap_or_else(|| DEFAULT_RUSTFS_API_PORT.to_string());
        Ok(Self {
            authority: format!("{host}:{port}"),
            region: text("region").unwrap_or_else(|| DEFAULT_REGION.to_string()),
            access_key: required("access_key")?,
            secret_key: required("secret_key")?,
        })
    }
}

fn failure(service_id: i32, what: &str, reason: impl std::fmt::Display) -> ExternalServiceError {
    ExternalServiceError::InternalError {
        reason: format!("Failed to {what} on S3 service {service_id}: {reason}"),
    }
}

impl S3SandboxAccess {
    async fn send(
        client: &reqwest::Client,
        endpoint: &ServiceEndpoint,
        service_id: i32,
        method: reqwest::Method,
        path: &str,
        form: Option<String>,
        service: &str,
    ) -> Result<(reqwest::StatusCode, String), ExternalServiceError> {
        let body = form.unwrap_or_default();
        let content_type = (!body.is_empty()).then_some("application/x-www-form-urlencoded");
        let headers = sign_request(
            method.as_str(),
            &endpoint.authority,
            path,
            content_type,
            body.as_bytes(),
            service,
            &endpoint.region,
            &endpoint.access_key,
            &endpoint.secret_key,
            Utc::now(),
        )
        .map_err(|error| failure(service_id, "sign a request", error))?;
        let mut request = client.request(
            method.clone(),
            format!("http://{}{path}", endpoint.authority),
        );
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if !body.is_empty() {
            request = request.body(body);
        }
        let response = request.send().await.map_err(|error| {
            failure(
                service_id,
                &format!("reach the service at {}", endpoint.authority),
                error,
            )
        })?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| failure(service_id, "read the service's answer", error))?;
        Ok((status, text))
    }
}

#[async_trait]
impl SandboxAccessEngine for S3SandboxAccess {
    fn service_types(&self) -> &'static [&'static str] {
        &["s3", "rustfs"]
    }

    async fn containers(
        &self,
        manager: &ExternalServiceManager,
        service: &external_services::Model,
    ) -> Result<Vec<String>, ExternalServiceError> {
        let parameters = manager.get_service_parameters(service.id).await?;
        if !is_rustfs_backed(service, &parameters) {
            return Ok(Vec::new());
        }
        Ok(vec![service
            .container_name
            .clone()
            .unwrap_or_else(|| rustfs_container_name(&service.name))])
    }

    async fn issue(
        &self,
        manager: &ExternalServiceManager,
        request: SandboxAccessRequest<'_>,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let SandboxAccessRequest {
            service,
            namespace,
            project_id,
            parameters,
        } = request;
        if !is_rustfs_backed(service, parameters) {
            return Err(ExternalServiceError::NoSandboxAccessEngine {
                service_id: service.id,
                service_type: service.service_type.clone(),
            });
        }
        let containers = self.containers(manager, service).await?;
        let container = containers.first().cloned().ok_or_else(|| {
            ExternalServiceError::NoSandboxAccessEngine {
                service_id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let bucket = sandbox_bucket_name(namespace).map_err(|error| {
            ExternalServiceError::SandboxAccessUnsupported {
                service_id: service.id,
                service_type: service.service_type.clone(),
                reason: error.to_string(),
            }
        })?;
        let endpoint = ServiceEndpoint::from_parameters(service, parameters)?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| failure(service.id, "build an HTTP client", error))?;

        // The bucket first, with the service's own key: the temporary key is
        // allowed to use it, never to create it.
        let bucket_path = format!("/{bucket}");
        let (status, _) = Self::send(
            &client,
            &endpoint,
            service.id,
            reqwest::Method::HEAD,
            &bucket_path,
            None,
            "s3",
        )
        .await?;
        if !status.is_success() {
            let (status, text) = Self::send(
                &client,
                &endpoint,
                service.id,
                reqwest::Method::PUT,
                &bucket_path,
                None,
                "s3",
            )
            .await?;
            let already_ours = text.contains("BucketAlreadyOwnedByYou");
            if !status.is_success() && !already_ours {
                return Err(failure(
                    service.id,
                    &format!("create the sandbox bucket '{bucket}'"),
                    format!("the service answered {status}: {}", truncate(&text)),
                ));
            }
        }

        let form = form_urlencoded(&[
            ("Action", "AssumeRole"),
            ("Version", "2011-06-15"),
            ("DurationSeconds", &CREDENTIAL_LIFETIME_SECS.to_string()),
            ("Policy", &session_policy(&bucket).to_string()),
        ]);
        let (status, text) = Self::send(
            &client,
            &endpoint,
            service.id,
            reqwest::Method::POST,
            "/",
            Some(form),
            "sts",
        )
        .await?;
        if !status.is_success() {
            return Err(failure(
                service.id,
                "issue a temporary credential",
                format!("the service answered {status}: {}", truncate(&text)),
            ));
        }
        let credentials = parse_assume_role_response(&text)
            .map_err(|error| failure(service.id, "issue a temporary credential", error))?;

        info!(
            service_id = service.id,
            project_id = ?project_id,
            bucket = %bucket,
            lifetime_secs = CREDENTIAL_LIFETIME_SECS,
            "Issued a bucket-scoped temporary credential for a sandbox"
        );
        Ok(environment(
            &container,
            DEFAULT_RUSTFS_API_PORT,
            &endpoint.region,
            &bucket,
            &credentials,
        ))
    }
}

fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// A service's error body, short enough for an error message.
fn truncate(text: &str) -> String {
    const LIMIT: usize = 300;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        trimmed.to_string()
    } else {
        let mut cut: String = trimmed.chars().take(LIMIT).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn s3_service(node_id: Option<i32>) -> external_services::Model {
        let now = Utc::now();
        external_services::Model {
            id: 2,
            name: "files".to_string(),
            service_type: "s3".to_string(),
            topology: "standalone".to_string(),
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            node_id,
            version: None,
            slug: None,
            config: None,
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    fn parameters(pairs: &[(&str, &str)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), serde_json::json!(value)))
            .collect()
    }

    #[test]
    fn only_a_local_rustfs_service_is_covered() {
        let service = s3_service(None);
        assert!(is_rustfs_backed(&service, &parameters(&[])));
        assert!(is_rustfs_backed(
            &service,
            &parameters(&[("server", "rustfs")])
        ));
        // A legacy MinIO server is addressed differently: left out, not dialled
        // as RustFS.
        assert!(!is_rustfs_backed(
            &service,
            &parameters(&[("server", "minio")])
        ));
        assert!(!is_rustfs_backed(
            &service,
            &parameters(&[("server", " MinIO ")])
        ));
        assert!(!is_rustfs_backed(
            &service,
            &parameters(&[("backend", "garage")])
        ));
        // A service on another node is out of reach.
        assert!(!is_rustfs_backed(&s3_service(Some(3)), &parameters(&[])));
    }

    fn credentials() -> TemporaryCredentials {
        TemporaryCredentials {
            access_key: "TMPACCESSKEY".to_string(),
            secret_key: "tmp-secret-value".to_string(),
            session_token: "tmp-session-token".to_string(),
        }
    }

    fn bucket(namespace: &SandboxNamespace) -> String {
        sandbox_bucket_name(namespace).expect("valid")
    }

    fn project(slug: &str) -> SandboxNamespace {
        SandboxNamespace::project(slug)
    }

    #[test]
    fn the_bucket_is_named_for_the_project_and_the_sandbox_scope() {
        assert_eq!(bucket(&project("shop-api")), "shop-api.sandbox");
        assert_eq!(
            bucket(&SandboxNamespace::sandbox("sbx_ab12cd34")),
            "ab12cd34.id.sandbox"
        );
    }

    #[test]
    fn no_deployment_bucket_can_be_named_like_a_sandbox_bucket() {
        use crate::externalsvc::managed_s3::{plan_buckets, scope_slug};
        // The old sandbox name was an environment slugged `sandbox`'s bucket.
        let environment = plan_buckets(&scope_slug("shop", "sandbox"), &[]);
        let project_plan = plan_buckets(&scope_slug("shop-sandbox", ""), &[]);
        let declared = plan_buckets(&scope_slug("shop", "prod"), &["sandbox".to_string()]);
        let sandbox = bucket(&project("shop"));
        for plan in environment.iter().chain(&project_plan).chain(&declared) {
            assert_ne!(plan.physical, sandbox);
            assert!(!plan.physical.contains('.'), "{}", plan.physical);
        }
        // Not even a slug or scope with dots in it produces one.
        assert!(!scope_slug("shop.sandbox", "").contains('.'));
    }

    #[test]
    fn a_project_slugged_like_a_sandbox_id_never_gets_that_sandboxs_bucket() {
        let sandbox = bucket(&SandboxNamespace::sandbox("sbx_ab12"));
        for slug in ["ab12", "sab12", "sbx-ab12", "ab12-id", "ab12.id"] {
            assert_ne!(bucket(&project(slug)), sandbox, "{slug}");
        }
        // Nor through the hashed forms: a project can never end in
        // `.id.sandbox` or `.idh.sandbox`, a sandbox never in a bare
        // `<label>.sandbox` or `.h.sandbox`.
        for slug in ["ab12", "café", &"a".repeat(63), "sbx_ab12", "x.id"] {
            let name = bucket(&project(slug));
            assert!(
                !name.ends_with(".id.sandbox") && !name.ends_with(".idh.sandbox"),
                "{name}"
            );
        }
        for id in [
            "sbx_ab12",
            "ab12",
            "sbx_AB12",
            &format!("sbx_{}", "a".repeat(70)),
        ] {
            let name = bucket(&SandboxNamespace::sandbox(id));
            assert!(
                name.ends_with(".id.sandbox") || name.ends_with(".idh.sandbox"),
                "{name}"
            );
        }
    }

    #[test]
    fn slugs_that_are_not_plain_still_get_a_valid_bucket_of_their_own() {
        // `slugify` keeps any Unicode letter, so accents reach here; folding
        // them would give `café` and `cafè` one bucket.
        let names: Vec<String> = [
            "café",
            "cafè",
            "caf",
            "a--b",
            "a-b",
            "shop-",
            "-shop",
            "",
            "xn--shop",
            "sthree-shop",
            "Shop_API",
            "bad/slug",
            "x y",
        ]
        .iter()
        .map(|slug| bucket(&project(slug)))
        .chain([
            bucket(&project(&"a".repeat(55))),
            bucket(&project(&"a".repeat(56))),
            bucket(&project(&"a".repeat(57))),
            bucket(&project(&"é".repeat(63))),
        ])
        .collect();
        for name in &names {
            assert!(is_valid_sandbox_bucket(name), "{name}");
            assert!(!name.contains(".-") && !name.contains("-.") && !name.contains(".."));
            assert!(
                !name.starts_with("xn--") && !name.starts_with("sthree-"),
                "{name}"
            );
        }
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        assert_eq!(names[2], "caf.sandbox");
        assert!(names[0].starts_with("caf-") && names[0].ends_with(".h.sandbox"));
    }

    #[test]
    fn the_session_policy_names_only_the_one_bucket() {
        let policy = session_policy("shop-api.sandbox");
        let statement = &policy["Statement"][0];
        assert_eq!(statement["Effect"], "Allow");
        let resources: Vec<&str> = statement["Resource"]
            .as_array()
            .expect("resources")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert_eq!(
            resources,
            [
                "arn:aws:s3:::shop-api.sandbox",
                "arn:aws:s3:::shop-api.sandbox/*"
            ]
        );
        let actions: Vec<&str> = statement["Action"]
            .as_array()
            .expect("actions")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert!(actions.contains(&"s3:GetObject") && actions.contains(&"s3:PutObject"));
        // Nothing that administers the bucket or other buckets.
        for action in &actions {
            assert_ne!(*action, "s3:*");
            assert!(!action.starts_with("s3:Delete") || action.contains("Object"));
            assert!(!action.contains("Policy") && !action.contains("CreateBucket"));
        }
    }

    #[test]
    fn the_signing_key_matches_the_documented_derivation() {
        // The worked example in AWS's "Derive a signing key" documentation.
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20120215",
            "us-east-1",
            "iam",
        )
        .expect("derives");
        assert_eq!(
            hex::encode(key),
            "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
        );
    }

    #[test]
    fn a_signed_request_names_its_credential_scope_and_headers() {
        let now = Utc.with_ymd_and_hms(2026, 10, 17, 12, 30, 5).unwrap();
        let headers = sign_request(
            "POST",
            "localhost:9000",
            "/",
            Some("application/x-www-form-urlencoded"),
            b"Action=AssumeRole",
            "sts",
            "us-east-1",
            "ROOTKEY",
            "root-secret",
            now,
        )
        .expect("signs");
        let get = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("x-amz-date"), "20261017T123005Z");
        let authorization = get("authorization");
        assert!(authorization.starts_with(
            "AWS4-HMAC-SHA256 Credential=ROOTKEY/20261017/us-east-1/sts/aws4_request, "
        ));
        assert!(authorization
            .contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, "));
        assert!(!authorization.contains("root-secret"));
        // Same inputs, same signature; a different body, a different one.
        let again = sign_request(
            "POST",
            "localhost:9000",
            "/",
            Some("application/x-www-form-urlencoded"),
            b"Action=AssumeRole",
            "sts",
            "us-east-1",
            "ROOTKEY",
            "root-secret",
            now,
        )
        .expect("signs");
        assert_eq!(headers, again);
        let other = sign_request(
            "POST",
            "localhost:9000",
            "/",
            Some("application/x-www-form-urlencoded"),
            b"Action=Other",
            "sts",
            "us-east-1",
            "ROOTKEY",
            "root-secret",
            now,
        )
        .expect("signs");
        assert_ne!(headers, other);
    }

    #[test]
    fn the_credential_is_read_from_the_sts_answer() {
        let xml = "<AssumeRoleResponse><AssumeRoleResult><Credentials>\
                   <AccessKeyId>AKTEMP</AccessKeyId><Expiration>2026-10-18T00:00:00Z</Expiration>\
                   <SecretAccessKey>secret</SecretAccessKey><SessionToken>token</SessionToken>\
                   </Credentials></AssumeRoleResult></AssumeRoleResponse>";
        let parsed = parse_assume_role_response(xml).expect("parses");
        assert_eq!(parsed.access_key, "AKTEMP");
        assert_eq!(parsed.secret_key, "secret");
        assert_eq!(parsed.session_token, "token");
    }

    #[test]
    fn an_sts_answer_without_a_credential_is_an_error_naming_the_missing_field() {
        for (xml, missing) in [
            ("<Error><Code>AccessDenied</Code></Error>", "AccessKeyId"),
            (
                "<AccessKeyId>a</AccessKeyId><SecretAccessKey>b</SecretAccessKey>",
                "SessionToken",
            ),
            (
                "<AccessKeyId></AccessKeyId><SecretAccessKey>b</SecretAccessKey>",
                "AccessKeyId",
            ),
            ("<AccessKeyId>a", "AccessKeyId"),
        ] {
            let error = parse_assume_role_response(xml).expect_err(xml);
            assert!(error.to_string().contains(missing), "{xml}: {error}");
        }
    }

    #[test]
    fn the_environment_uses_the_deployment_names_and_adds_the_session_token() {
        let env = environment(
            "rustfs-store",
            9000,
            "us-east-1",
            "shop-api.sandbox",
            &credentials(),
        );
        for name in [
            "S3_BUCKET",
            "S3_ENDPOINT",
            "S3_HOST",
            "S3_PORT",
            "S3_ACCESS_KEY",
            "S3_SECRET_KEY",
            "S3_REGION",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_DEFAULT_REGION",
            "AWS_ENDPOINT_URL",
        ] {
            assert!(env.contains_key(name), "missing {name}");
        }
        assert_eq!(env["S3_ENDPOINT"], "http://rustfs-store:9000");
        assert_eq!(env["AWS_ENDPOINT_URL"], "http://rustfs-store:9000");
        assert_eq!(env["S3_HOST"], "rustfs-store");
        assert_eq!(env["S3_PORT"], "9000");
        assert_eq!(env["AWS_SESSION_TOKEN"], "tmp-session-token");
        assert_eq!(env["S3_SESSION_TOKEN"], "tmp-session-token");
        assert_eq!(env["S3_BUCKET"], "shop-api.sandbox");
    }

    #[test]
    fn the_debug_output_hides_the_credential() {
        let shown = format!("{:?}", credentials());
        for secret in ["TMPACCESSKEY", "tmp-secret-value", "tmp-session-token"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }

    #[test]
    fn the_form_is_percent_encoded() {
        assert_eq!(
            form_urlencoded(&[("Policy", "{\"a\": \"b c\"}"), ("Action", "AssumeRole")]),
            "Policy=%7B%22a%22%3A%20%22b%20c%22%7D&Action=AssumeRole"
        );
    }

    #[test]
    fn an_error_body_is_cut_to_a_readable_length() {
        let long = "x".repeat(1000);
        let cut = truncate(&long);
        assert!(cut.chars().count() <= 301);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate("  short  "), "short");
    }
}
