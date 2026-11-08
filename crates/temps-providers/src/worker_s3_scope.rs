// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The only S3 credential a worker node ever receives (ADR-093).
//!
//! A backup or restore of a service on a worker node runs on that node, by its
//! agent, and needs to reach the S3 source. It never receives the source's
//! own credential: the control plane derives one for the job, limited to the
//! job's key prefixes, to reading or writing, and to a lifetime, and that is
//! what the sealed agent request carries. Every call site that sends a
//! [`WorkerS3Destination`] gets it from [`WorkerCredentialScoper::destination`].
//!
//! How a credential is derived depends on the source:
//!
//! - Temps Cloud managed backups: Cloud mints it
//!   ([`temps_core::ManagedBackupScopedCredentials`]);
//! - AWS S3: `sts:GetFederationToken` with a session policy;
//! - any other endpoint: the store's own STS `AssumeRole` with the same
//!   session policy (MinIO, RustFS);
//! - a store that cannot (Garage, an R2 bucket's own keys, an STS that
//!   rejects the call): the job is refused, unless the source's
//!   `worker_credentials` is `allow_full`, in which case the worker gets the
//!   source's credential and every job says so in a warning.
//!
//! No job needs `DeleteObject`, so no session policy grants it. Retention and
//! deletion run on the control plane.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use temps_core::{
    ManagedBackupScopedCredentials, ScopedS3Access, ScopedS3CredentialError,
    ScopedS3CredentialRequest,
};
use temps_entities::s3_sources;
use temps_service_protocol::WorkerS3Destination;
use thiserror::Error;
use tracing::{info, warn};

/// How long a one-shot job's credential lives: longer than any backup's or
/// restore's runtime limit.
pub const JOB_CREDENTIAL_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
/// How long a continuous-archiving credential lives. It sits in `walg.env`
/// between base backups and is refreshed every
/// [`ARCHIVE_CREDENTIAL_REFRESH_INTERVAL`].
pub const ARCHIVE_CREDENTIAL_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the control plane refreshes a worker's archiving credential. A
/// control plane down for less than the lifetime minus this loses no WAL.
/// Two hours, because Temps Cloud caps a managed source's credentials at six
/// (its token rotation overlap), which leaves four hours of tolerance there
/// and twenty-two with a store that grants the full lifetime.
pub const ARCHIVE_CREDENTIAL_REFRESH_INTERVAL: Duration = Duration::from_secs(2 * 60 * 60);
/// Bound on one STS call.
const STS_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_REGION: &str = "us-east-1";
/// AWS's `GetFederationToken` lifetime bounds.
const AWS_MIN_LIFETIME_SECS: u64 = 900;
const AWS_MAX_LIFETIME_SECS: u64 = 129_600;

/// What one worker job may do, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerS3Grant {
    pub service_id: i32,
    pub node_id: i32,
    /// Object-key prefixes inside the source's bucket, each ending in `/`.
    pub prefixes: Vec<String>,
    pub access: ScopedS3Access,
    pub lifetime: Duration,
    /// What the job is, for errors and logs ("backup", "restore",
    /// "continuous archiving").
    pub purpose: &'static str,
}

impl WorkerS3Grant {
    /// A one-shot job on `prefix`.
    pub fn job(
        service_id: i32,
        node_id: i32,
        prefix: impl Into<String>,
        access: ScopedS3Access,
        purpose: &'static str,
    ) -> Self {
        Self {
            service_id,
            node_id,
            prefixes: vec![prefix.into()],
            access,
            lifetime: JOB_CREDENTIAL_LIFETIME,
            purpose,
        }
    }

    /// A continuous-archiving credential on `prefix`: written, and living
    /// [`ARCHIVE_CREDENTIAL_LIFETIME`].
    pub fn archive(service_id: i32, node_id: i32, prefix: impl Into<String>) -> Self {
        Self {
            service_id,
            node_id,
            prefixes: vec![prefix.into()],
            access: ScopedS3Access::Write,
            lifetime: ARCHIVE_CREDENTIAL_LIFETIME,
            purpose: "continuous archiving",
        }
    }
}

/// The directory an object key lives in, with a trailing slash: the prefix a
/// job writing or reading that one object is scoped to.
pub fn key_directory(key: &str) -> String {
    let key = key.trim_start_matches('/');
    match key.rsplit_once('/') {
        Some((directory, _)) => format!("{directory}/"),
        None => String::new(),
    }
}

/// A prefix with exactly one trailing slash and no leading one.
pub fn as_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

/// The object-key prefix of an `s3://bucket/key/prefix` URL inside `bucket`.
pub fn s3_url_prefix(url: &str, bucket: &str) -> Option<String> {
    let rest = url.strip_prefix("s3://")?;
    let (url_bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    (url_bucket == bucket).then(|| as_prefix(key))
}

/// How the credential a worker received was obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerCredentialScope {
    /// Derived for the job.
    Scoped {
        minted_by: &'static str,
        expires_at: Option<DateTime<Utc>>,
    },
    /// The source's own credential, because the source cannot scope and its
    /// `worker_credentials` allows it.
    Full { reason: String },
}

/// A destination for a worker job and how its credential was obtained.
#[derive(Debug, Clone)]
pub struct ScopedWorkerDestination {
    pub destination: WorkerS3Destination,
    pub scope: WorkerCredentialScope,
}

impl ScopedWorkerDestination {
    /// A line for the job's own log when the worker holds the source's full
    /// credential; `None` for a scoped one.
    pub fn warning(&self, s3_source_name: &str, node_id: i32) -> Option<String> {
        match &self.scope {
            WorkerCredentialScope::Scoped { .. } => None,
            WorkerCredentialScope::Full { reason } => Some(format!(
                "Worker node {node_id} received S3 source '{s3_source_name}''s full credential \
                 (it cannot scope one: {reason}); the source's worker credentials setting \
                 allows this. That node can read and delete every object in bucket '{}'.",
                self.destination.bucket
            )),
        }
    }
}

#[derive(Debug, Error)]
pub enum WorkerCredentialError {
    #[error(
        "the {purpose} of service {service_id} asked for an invalid credential scope on S3 \
         source {s3_source_id}: {reason}"
    )]
    InvalidGrant {
        purpose: &'static str,
        service_id: i32,
        s3_source_id: i32,
        reason: String,
    },
    #[error("could not decrypt the {field} of S3 source {s3_source_id}: {reason}")]
    Decrypt {
        s3_source_id: i32,
        field: &'static str,
        reason: String,
    },
    #[error(
        "S3 source {s3_source_id} ('{source_name}') cannot give worker node {node_id} a \
         credential limited to the {purpose} of service {service_id}: {reason}. Worker nodes \
         never receive a source's own credential unless the source allows it: either set \
         'Worker credentials' to 'Allow full credentials' on the source (Backups, S3 sources, \
         '{source_name}'), accepting that node {node_id} can then read and delete every backup \
         in bucket '{bucket}', or back up to a store that can scope credentials (AWS S3, MinIO \
         or RustFS with STS, Temps Cloud managed backups)"
    )]
    Unscopable {
        s3_source_id: i32,
        source_name: String,
        bucket: String,
        service_id: i32,
        node_id: i32,
        purpose: &'static str,
        reason: String,
    },
    #[error(
        "Temps Cloud did not mint a scoped credential for the {purpose} of service \
         {service_id} on worker node {node_id} (managed S3 source {s3_source_id}): {source}. \
         The worker never receives the managed bucket's own token"
    )]
    Managed {
        s3_source_id: i32,
        service_id: i32,
        node_id: i32,
        purpose: &'static str,
        #[source]
        source: ScopedS3CredentialError,
    },
    #[error(
        "the STS of S3 source {s3_source_id} at {endpoint} did not mint a scoped credential \
         for the {purpose} of service {service_id} on worker node {node_id}: {reason}; a retry \
         may succeed"
    )]
    Transient {
        s3_source_id: i32,
        endpoint: String,
        service_id: i32,
        node_id: i32,
        purpose: &'static str,
        reason: String,
    },
}

impl WorkerCredentialError {
    /// Whether retrying the job may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transient { .. } => true,
            Self::Managed { source, .. } => {
                matches!(source, ScopedS3CredentialError::Transient { .. })
            }
            Self::InvalidGrant { .. } | Self::Decrypt { .. } | Self::Unscopable { .. } => false,
        }
    }
}

/// The IAM session policy of a grant: the object operations `access` needs
/// on `prefixes` of `bucket`, listing limited to them, and nothing else. It
/// intersects with what the source's key may do, so it can only narrow it.
pub fn session_policy(bucket: &str, prefixes: &[String], access: ScopedS3Access) -> String {
    let objects: Vec<String> = prefixes
        .iter()
        .map(|prefix| format!("arn:aws:s3:::{bucket}/{prefix}*"))
        .collect();
    let mut listed: Vec<String> = Vec::new();
    for prefix in prefixes {
        listed.push(format!("{prefix}*"));
        // A client listing the directory itself names it without the slash.
        listed.push(prefix.trim_end_matches('/').to_string());
    }
    let object_actions: &[&str] = match access {
        ScopedS3Access::Read => &["s3:GetObject"],
        ScopedS3Access::Write => &[
            "s3:PutObject",
            "s3:GetObject",
            "s3:AbortMultipartUpload",
            "s3:ListMultipartUploadParts",
        ],
    };
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": object_actions,
                "Resource": objects,
            },
            {
                "Effect": "Allow",
                "Action": ["s3:ListBucket"],
                "Resource": [format!("arn:aws:s3:::{bucket}")],
                "Condition": { "StringLike": { "s3:prefix": listed } },
            },
            {
                "Effect": "Allow",
                "Action": ["s3:GetBucketLocation"],
                "Resource": [format!("arn:aws:s3:::{bucket}")],
            },
        ],
    })
    .to_string()
}

/// Which STS a source's credential can be derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StsTarget {
    /// AWS: `GetFederationToken` at the region's STS endpoint.
    AwsFederation {
        url: String,
        host: String,
        region: String,
    },
    /// Another store: its own `AssumeRole` at the S3 endpoint.
    AssumeRole {
        url: String,
        host: String,
        region: String,
    },
    /// Nothing can derive one.
    None { reason: String },
}

impl StsTarget {
    fn url(&self) -> &str {
        match self {
            Self::AwsFederation { url, .. } | Self::AssumeRole { url, .. } => url,
            Self::None { .. } => "",
        }
    }
}

fn region_or_default(region: &str) -> String {
    let region = region.trim();
    if region.is_empty() || region.eq_ignore_ascii_case("auto") {
        DEFAULT_REGION.to_string()
    } else {
        region.to_string()
    }
}

/// The STS that can derive a credential for `source`.
pub fn sts_target(endpoint: Option<&str>, region: &str) -> StsTarget {
    let region = region_or_default(region);
    let aws = |region: String| StsTarget::AwsFederation {
        url: format!("https://sts.{region}.amazonaws.com/"),
        host: format!("sts.{region}.amazonaws.com"),
        region,
    };
    let Some(endpoint) = endpoint.map(str::trim).filter(|e| !e.is_empty()) else {
        return aws(region);
    };
    let (scheme, rest) = match endpoint.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("https".to_string(), endpoint),
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let hostname = host
        .strip_prefix('[')
        .and_then(|h| h.split_once(']').map(|(inner, _)| inner))
        .unwrap_or_else(|| host.split(':').next().unwrap_or(host))
        .to_ascii_lowercase();
    if hostname.is_empty() || (scheme != "http" && scheme != "https") {
        return StsTarget::None {
            reason: format!("its endpoint '{endpoint}' is not an http(s) URL"),
        };
    }
    if hostname.ends_with(".amazonaws.com") {
        return aws(region);
    }
    if hostname.ends_with(".r2.cloudflarestorage.com") {
        return StsTarget::None {
            reason: "it is a Cloudflare R2 bucket, whose S3 keys cannot mint scoped \
                     credentials (only Cloudflare's account API can)"
                .to_string(),
        };
    }
    StsTarget::AssumeRole {
        url: format!("{scheme}://{host}/"),
        host: host.to_string(),
        region,
    }
}

/// The text of `<tag>` in an STS answer.
fn xml_field(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let length = xml[start..].find(&close)?;
    let value = xml[start..start + length].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// `Code: Message` of an STS or S3 error answer, or its first characters.
fn describe_error_body(body: &str) -> String {
    match (xml_field(body, "Code"), xml_field(body, "Message")) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code,
        _ => body.chars().take(200).collect(),
    }
}

/// A credential minted by an STS.
struct StsCredential {
    access_key_id: String,
    secret_access_key: String,
    session_token: String,
    expires_at: Option<DateTime<Utc>>,
}

/// Why an STS call did not mint.
enum StsFailure {
    /// Retrying cannot help: the store does not do this.
    Unsupported(String),
    /// A retry may.
    Transient(String),
}

/// The form an STS call posts.
fn sts_form(target: &StsTarget, grant: &WorkerS3Grant, policy: &str) -> String {
    let lifetime = grant.lifetime.as_secs();
    let pairs: Vec<(&str, String)> = match target {
        StsTarget::AwsFederation { .. } => vec![
            ("Action", "GetFederationToken".to_string()),
            ("Version", "2011-06-15".to_string()),
            // `[\w+=,.@-]{2,32}`: shows in CloudTrail as who used it.
            ("Name", format!("temps-svc-{}", grant.service_id)),
            (
                "DurationSeconds",
                lifetime
                    .clamp(AWS_MIN_LIFETIME_SECS, AWS_MAX_LIFETIME_SECS)
                    .to_string(),
            ),
            ("Policy", policy.to_string()),
        ],
        StsTarget::AssumeRole { .. } | StsTarget::None { .. } => vec![
            ("Action", "AssumeRole".to_string()),
            ("Version", "2011-06-15".to_string()),
            (
                "DurationSeconds",
                lifetime.max(AWS_MIN_LIFETIME_SECS).to_string(),
            ),
            ("Policy", policy.to_string()),
        ],
    };
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

/// Derives every worker job's S3 credential. One per process, shared by the
/// backup engines and the archive re-point.
pub struct WorkerCredentialScoper {
    encryption: Arc<temps_core::EncryptionService>,
    http: reqwest::Client,
    /// Filled once every plugin has registered (the Cloud plugin registers the
    /// minter). Empty: no Cloud plugin, so a managed source cannot be scoped.
    managed: OnceLock<Arc<dyn ManagedBackupScopedCredentials>>,
}

impl WorkerCredentialScoper {
    pub fn new(encryption: Arc<temps_core::EncryptionService>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(STS_TIMEOUT)
            .connect_timeout(STS_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            encryption,
            http,
            managed: OnceLock::new(),
        }
    }

    /// Hand over the Cloud plugin's minter. Called once at initialisation;
    /// a second call is ignored.
    pub fn set_managed_minter(&self, minter: Arc<dyn ManagedBackupScopedCredentials>) {
        let _ = self.managed.set(minter);
    }

    /// The destination a worker job on `source` receives: a credential
    /// derived for `grant`, or the job's refusal.
    pub async fn destination(
        &self,
        source: &s3_sources::Model,
        grant: WorkerS3Grant,
    ) -> Result<ScopedWorkerDestination, WorkerCredentialError> {
        validate_grant(source, &grant)?;
        let base = WorkerS3Destination {
            bucket: source.bucket_name.clone(),
            region: source.region.clone(),
            endpoint: source.endpoint.clone(),
            force_path_style: source.force_path_style.unwrap_or(true),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            session_token: None,
        };

        if source.managed_by_cloud {
            return self.managed_destination(source, &grant, base).await;
        }

        let (access_key_id, secret_access_key) = self.decrypt_pair(source)?;
        let source_token =
            s3_sources::decrypt_session_token(&self.encryption, source).map_err(|error| {
                WorkerCredentialError::Decrypt {
                    s3_source_id: source.id,
                    field: "session token",
                    reason: error.to_string(),
                }
            })?;
        let target = sts_target(source.endpoint.as_deref(), &source.region);
        let minted = match (&target, source_token.as_ref()) {
            (StsTarget::None { reason }, _) => Err(StsFailure::Unsupported(reason.clone())),
            (_, Some(_)) => Err(StsFailure::Unsupported(
                "its own credential is temporary (it carries a session token), and a \
                 temporary credential cannot mint another"
                    .to_string(),
            )),
            (_, None) => {
                self.call_sts(&target, source, &grant, &access_key_id, &secret_access_key)
                    .await
            }
        };
        match minted {
            Ok(credential) => {
                info!(
                    s3_source_id = source.id,
                    service_id = grant.service_id,
                    node_id = grant.node_id,
                    purpose = grant.purpose,
                    access = grant.access.as_str(),
                    prefixes = ?grant.prefixes,
                    expires_at = ?credential.expires_at,
                    "Minted a scoped S3 credential for a worker job"
                );
                Ok(ScopedWorkerDestination {
                    destination: WorkerS3Destination {
                        access_key_id: credential.access_key_id,
                        secret_access_key: credential.secret_access_key,
                        session_token: Some(credential.session_token),
                        ..base
                    },
                    scope: WorkerCredentialScope::Scoped {
                        minted_by: match target {
                            StsTarget::AwsFederation { .. } => "AWS STS",
                            _ => "the store's STS",
                        },
                        expires_at: credential.expires_at,
                    },
                })
            }
            Err(StsFailure::Transient(reason)) => Err(WorkerCredentialError::Transient {
                s3_source_id: source.id,
                endpoint: target.url().to_string(),
                service_id: grant.service_id,
                node_id: grant.node_id,
                purpose: grant.purpose,
                reason,
            }),
            Err(StsFailure::Unsupported(reason))
                if source.worker_credentials == s3_sources::WORKER_CREDENTIALS_ALLOW_FULL =>
            {
                warn!(
                    s3_source_id = source.id,
                    service_id = grant.service_id,
                    node_id = grant.node_id,
                    purpose = grant.purpose,
                    reason = %reason,
                    "Sending S3 source {}'s full credential to worker node {}: it cannot scope \
                     one and its worker credentials setting allows full credentials",
                    source.id,
                    grant.node_id
                );
                Ok(ScopedWorkerDestination {
                    destination: WorkerS3Destination {
                        access_key_id,
                        secret_access_key,
                        session_token: None,
                        ..base
                    },
                    scope: WorkerCredentialScope::Full { reason },
                })
            }
            Err(StsFailure::Unsupported(reason)) => Err(WorkerCredentialError::Unscopable {
                s3_source_id: source.id,
                source_name: source.name.clone(),
                bucket: source.bucket_name.clone(),
                service_id: grant.service_id,
                node_id: grant.node_id,
                purpose: grant.purpose,
                reason,
            }),
        }
    }

    async fn managed_destination(
        &self,
        source: &s3_sources::Model,
        grant: &WorkerS3Grant,
        base: WorkerS3Destination,
    ) -> Result<ScopedWorkerDestination, WorkerCredentialError> {
        let managed_error = |error: ScopedS3CredentialError| WorkerCredentialError::Managed {
            s3_source_id: source.id,
            service_id: grant.service_id,
            node_id: grant.node_id,
            purpose: grant.purpose,
            source: error,
        };
        let minter = self
            .managed
            .get()
            .ok_or_else(|| managed_error(ScopedS3CredentialError::NotConnected))?;
        let credential = minter
            .mint(ScopedS3CredentialRequest {
                prefixes: grant.prefixes.clone(),
                access: grant.access,
                lifetime: grant.lifetime,
            })
            .await
            .map_err(managed_error)?;
        info!(
            s3_source_id = source.id,
            service_id = grant.service_id,
            node_id = grant.node_id,
            purpose = grant.purpose,
            access = grant.access.as_str(),
            prefixes = ?grant.prefixes,
            expires_at = %credential.expires_at,
            "Temps Cloud minted a scoped S3 credential for a worker job"
        );
        Ok(ScopedWorkerDestination {
            destination: WorkerS3Destination {
                access_key_id: credential.access_key_id,
                secret_access_key: credential.secret_access_key,
                session_token: credential.session_token,
                ..base
            },
            scope: WorkerCredentialScope::Scoped {
                minted_by: "Temps Cloud",
                expires_at: Some(credential.expires_at),
            },
        })
    }

    fn decrypt_pair(
        &self,
        source: &s3_sources::Model,
    ) -> Result<(String, String), WorkerCredentialError> {
        let decrypt = |field: &'static str, value: &str| {
            self.encryption
                .decrypt_string(value)
                .map_err(|error| WorkerCredentialError::Decrypt {
                    s3_source_id: source.id,
                    field,
                    reason: error.to_string(),
                })
        };
        Ok((
            decrypt("access key", &source.access_key_id)?,
            decrypt("secret key", &source.secret_key)?,
        ))
    }

    async fn call_sts(
        &self,
        target: &StsTarget,
        source: &s3_sources::Model,
        grant: &WorkerS3Grant,
        access_key_id: &str,
        secret_access_key: &str,
    ) -> Result<StsCredential, StsFailure> {
        let (url, host, region) = match target {
            StsTarget::AwsFederation { url, host, region }
            | StsTarget::AssumeRole { url, host, region } => (url, host, region),
            StsTarget::None { reason } => return Err(StsFailure::Unsupported(reason.clone())),
        };
        let policy = session_policy(&source.bucket_name, &grant.prefixes, grant.access);
        let form = sts_form(target, grant, &policy);
        let content_type = "application/x-www-form-urlencoded";
        let headers = crate::sandbox_s3::sign_request(
            "POST",
            host,
            "/",
            Some(content_type),
            form.as_bytes(),
            "sts",
            region,
            access_key_id,
            secret_access_key,
            Utc::now(),
        )
        .map_err(|error| {
            StsFailure::Unsupported(format!("could not sign the STS call: {error}"))
        })?;
        let mut request = self.http.post(url.as_str());
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .body(form)
            .send()
            .await
            .map_err(|error| StsFailure::Transient(error.without_url().to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| StsFailure::Transient(error.without_url().to_string()))?;
        if !status.is_success() {
            let described = format!("its STS answered {status}: {}", describe_error_body(&body));
            // 501 Not Implemented is a store without STS; any other 5xx (and
            // 429) is the store failing for now.
            return if status.as_u16() == 429 || (status.is_server_error() && status.as_u16() != 501)
            {
                Err(StsFailure::Transient(described))
            } else {
                Err(StsFailure::Unsupported(match target {
                    StsTarget::AwsFederation { .. } => {
                        format!("{described} (its IAM user needs sts:GetFederationToken)")
                    }
                    _ => described,
                }))
            };
        }
        let field = |tag: &str| {
            xml_field(&body, tag).ok_or_else(|| {
                StsFailure::Unsupported(format!(
                    "its STS answered {status} without a credential (no <{tag}>): {}",
                    describe_error_body(&body)
                ))
            })
        };
        Ok(StsCredential {
            access_key_id: field("AccessKeyId")?,
            secret_access_key: field("SecretAccessKey")?,
            session_token: field("SessionToken")?,
            expires_at: xml_field(&body, "Expiration")
                .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
                .map(|value| value.with_timezone(&Utc)),
        })
    }
}

/// Refuse a grant that could reach outside the job: no prefix, the whole
/// bucket, a `..` segment, or a prefix outside the source's own path.
fn validate_grant(
    source: &s3_sources::Model,
    grant: &WorkerS3Grant,
) -> Result<(), WorkerCredentialError> {
    let invalid = |reason: String| WorkerCredentialError::InvalidGrant {
        purpose: grant.purpose,
        service_id: grant.service_id,
        s3_source_id: source.id,
        reason,
    };
    if grant.prefixes.is_empty() {
        return Err(invalid("no key prefix".to_string()));
    }
    if grant.lifetime.is_zero() {
        return Err(invalid("a zero lifetime".to_string()));
    }
    let base = as_prefix(&source.bucket_path);
    for prefix in &grant.prefixes {
        if prefix.is_empty() || !prefix.ends_with('/') || prefix.starts_with('/') {
            return Err(invalid(format!(
                "prefix '{prefix}' must be a non-empty key directory ending in '/'"
            )));
        }
        if prefix
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
            || prefix.contains("//")
            || prefix.contains('*')
            || prefix.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "prefix '{prefix}' is not a plain key directory"
            )));
        }
        if !prefix.starts_with(&base) || prefix == &base {
            return Err(invalid(format!(
                "prefix '{prefix}' is not a directory inside the source's path '{base}'"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(endpoint: Option<&str>, bucket_path: &str) -> s3_sources::Model {
        let now = Utc::now();
        s3_sources::Model {
            id: 4,
            name: "offsite".to_string(),
            bucket_name: "backups".to_string(),
            region: "eu-west-1".to_string(),
            endpoint: endpoint.map(str::to_string),
            bucket_path: bucket_path.to_string(),
            access_key_id: String::new(),
            secret_key: String::new(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            worker_credentials: s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
            created_at: now,
            updated_at: now,
            backing_service_id: None,
        }
    }

    fn grant(prefix: &str) -> WorkerS3Grant {
        WorkerS3Grant::job(7, 2, prefix, ScopedS3Access::Write, "backup")
    }

    /// A managed source's archive credential lives at most
    /// `MANAGED_SCOPED_CREDENTIAL_MAX_LIFETIME`; refreshing at a third of it
    /// leaves two refreshes of slack before WAL archiving stops.
    #[test]
    fn the_archive_credential_is_refreshed_well_inside_every_lifetime() {
        assert!(
            ARCHIVE_CREDENTIAL_REFRESH_INTERVAL * 3
                <= temps_core::MANAGED_SCOPED_CREDENTIAL_MAX_LIFETIME
        );
        assert!(ARCHIVE_CREDENTIAL_REFRESH_INTERVAL * 3 <= ARCHIVE_CREDENTIAL_LIFETIME);
    }

    #[test]
    fn a_key_scopes_to_its_own_directory() {
        assert_eq!(
            key_directory("base/external_services/redis/cache/2026/11/08/u-1/dump.rdb.gz"),
            "base/external_services/redis/cache/2026/11/08/u-1/"
        );
        assert_eq!(key_directory("dump.rdb"), "");
        assert_eq!(as_prefix("/a/b/"), "a/b/");
        assert_eq!(as_prefix(""), "");
        assert_eq!(
            s3_url_prefix(
                "s3://backups/base/external_services/postgres/db/walg",
                "backups"
            ),
            Some("base/external_services/postgres/db/walg/".to_string())
        );
        assert_eq!(s3_url_prefix("s3://other/x", "backups"), None);
    }

    #[test]
    fn a_grant_must_stay_inside_the_sources_path() {
        let source = source(None, "base");
        assert!(validate_grant(&source, &grant("base/external_services/redis/cache/")).is_ok());
        for bad in [
            "",
            "base/",
            "other/external_services/redis/cache/",
            "base/external_services/redis/cache",
            "base/external_services/../postgres/",
            "base//x/",
            "base/x*/",
            "/base/x/",
        ] {
            let error = validate_grant(&source, &grant(bad)).expect_err(bad);
            assert!(
                matches!(
                    error,
                    WorkerCredentialError::InvalidGrant {
                        service_id: 7,
                        s3_source_id: 4,
                        ..
                    }
                ),
                "{bad}: {error}"
            );
        }
        let mut empty = grant("base/x/");
        empty.prefixes.clear();
        assert!(validate_grant(&source, &empty).is_err());
        // A source without a path still refuses the whole bucket.
        let root = self::source(None, "");
        assert!(validate_grant(&root, &grant("external_services/redis/cache/")).is_ok());
    }

    #[test]
    fn the_session_policy_grants_the_prefix_and_never_delete() {
        let prefixes = vec!["base/external_services/redis/cache/".to_string()];
        let write: serde_json::Value =
            serde_json::from_str(&session_policy("backups", &prefixes, ScopedS3Access::Write))
                .unwrap();
        let text = write.to_string();
        assert!(!text.contains("Delete"), "{text}");
        assert!(!text.contains("arn:aws:s3:::backups/*"), "{text}");
        assert_eq!(
            write["Statement"][0]["Resource"][0],
            "arn:aws:s3:::backups/base/external_services/redis/cache/*"
        );
        assert!(write["Statement"][0]["Action"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("s3:PutObject")));
        assert_eq!(
            write["Statement"][1]["Condition"]["StringLike"]["s3:prefix"],
            serde_json::json!([
                "base/external_services/redis/cache/*",
                "base/external_services/redis/cache"
            ])
        );
        let read: serde_json::Value =
            serde_json::from_str(&session_policy("backups", &prefixes, ScopedS3Access::Read))
                .unwrap();
        assert_eq!(
            read["Statement"][0]["Action"],
            serde_json::json!(["s3:GetObject"])
        );
    }

    #[test]
    fn each_kind_of_endpoint_has_its_sts() {
        assert_eq!(
            sts_target(None, "eu-west-1"),
            StsTarget::AwsFederation {
                url: "https://sts.eu-west-1.amazonaws.com/".to_string(),
                host: "sts.eu-west-1.amazonaws.com".to_string(),
                region: "eu-west-1".to_string(),
            }
        );
        assert!(matches!(
            sts_target(Some("https://s3.eu-west-1.amazonaws.com"), "eu-west-1"),
            StsTarget::AwsFederation { .. }
        ));
        assert_eq!(
            sts_target(Some("http://10.0.0.5:9000"), ""),
            StsTarget::AssumeRole {
                url: "http://10.0.0.5:9000/".to_string(),
                host: "10.0.0.5:9000".to_string(),
                region: "us-east-1".to_string(),
            }
        );
        assert!(matches!(
            sts_target(Some("https://abc.r2.cloudflarestorage.com"), "auto"),
            StsTarget::None { .. }
        ));
        assert!(matches!(
            sts_target(Some("ftp://store"), "x"),
            StsTarget::None { .. }
        ));
    }

    #[test]
    fn the_aws_form_names_the_service_and_bounds_the_lifetime() {
        let target = sts_target(None, "eu-west-1");
        let mut grant = grant("base/x/");
        grant.lifetime = Duration::from_secs(10 * 24 * 60 * 60);
        let form = sts_form(&target, &grant, "{}");
        assert!(form.contains("Action=GetFederationToken"), "{form}");
        assert!(form.contains("Name=temps-svc-7"), "{form}");
        assert!(form.contains("DurationSeconds=129600"), "{form}");
        let assume = sts_form(&sts_target(Some("http://store:9000"), ""), &grant, "{}");
        assert!(assume.contains("Action=AssumeRole"), "{assume}");
        assert!(!assume.contains("Name="), "{assume}");
    }

    #[test]
    fn an_unscopable_refusal_names_the_setting_and_the_risk() {
        let error = WorkerCredentialError::Unscopable {
            s3_source_id: 4,
            source_name: "offsite".to_string(),
            bucket: "backups".to_string(),
            service_id: 7,
            node_id: 2,
            purpose: "backup",
            reason: "its STS answered 501".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("Allow full credentials"), "{text}");
        assert!(text.contains("node 2"), "{text}");
        assert!(text.contains("bucket 'backups'"), "{text}");
        assert!(!error.is_retryable());
    }

    #[test]
    fn sts_errors_are_described_from_their_xml() {
        let body = "<ErrorResponse><Error><Code>AccessDenied</Code><Message>not allowed</Message></Error></ErrorResponse>";
        assert_eq!(describe_error_body(body), "AccessDenied: not allowed");
        assert_eq!(describe_error_body("plain"), "plain");
    }

    struct FixedMinter(Result<temps_core::ScopedS3Credential, ScopedS3CredentialError>);

    #[async_trait::async_trait]
    impl ManagedBackupScopedCredentials for FixedMinter {
        async fn mint(
            &self,
            _request: ScopedS3CredentialRequest,
        ) -> Result<temps_core::ScopedS3Credential, ScopedS3CredentialError> {
            self.0.clone()
        }
    }

    fn encryption() -> Arc<temps_core::EncryptionService> {
        Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn a_managed_source_sends_only_what_cloud_minted() {
        let scoper = WorkerCredentialScoper::new(encryption());
        let mut managed = source(
            Some("https://abc.r2.cloudflarestorage.com"),
            "t/managed-backups",
        );
        managed.managed_by_cloud = true;
        managed.access_key_id = "bucket-token-ciphertext".to_string();
        // No minter: refused, never the bucket token.
        let error = scoper
            .destination(
                &managed,
                grant("t/managed-backups/external_services/redis/c/"),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                WorkerCredentialError::Managed {
                    source: ScopedS3CredentialError::NotConnected,
                    ..
                }
            ),
            "{error}"
        );
        scoper.set_managed_minter(Arc::new(FixedMinter(Ok(temps_core::ScopedS3Credential {
            access_key_id: "scoped-id".to_string(),
            secret_access_key: "scoped-secret".to_string(),
            session_token: Some("scoped-token".to_string()),
            expires_at: Utc::now(),
        }))));
        let scoped = scoper
            .destination(
                &managed,
                grant("t/managed-backups/external_services/redis/c/"),
            )
            .await
            .unwrap();
        assert_eq!(scoped.destination.access_key_id, "scoped-id");
        assert_eq!(
            scoped.destination.session_token.as_deref(),
            Some("scoped-token")
        );
        assert_eq!(scoped.destination.bucket, "backups");
        assert!(scoped.warning("x", 2).is_none());
    }

    #[tokio::test]
    async fn an_older_cloud_refuses_the_job() {
        let scoper = WorkerCredentialScoper::new(encryption());
        scoper.set_managed_minter(Arc::new(FixedMinter(Err(
            ScopedS3CredentialError::Unsupported { status: 404 },
        ))));
        let mut managed = source(None, "t/managed-backups");
        managed.managed_by_cloud = true;
        let error = scoper
            .destination(
                &managed,
                grant("t/managed-backups/external_services/redis/c/"),
            )
            .await
            .unwrap_err();
        assert!(!error.is_retryable());
        assert!(error.to_string().contains("never receives"), "{error}");
    }

    #[tokio::test]
    async fn an_unscopable_source_refuses_unless_it_allows_full_credentials() {
        let encryption = encryption();
        let scoper = WorkerCredentialScoper::new(encryption.clone());
        let mut r2 = source(Some("https://abc.r2.cloudflarestorage.com"), "base");
        r2.access_key_id = encryption.encrypt_string("source-key").unwrap();
        r2.secret_key = encryption.encrypt_string("source-secret").unwrap();
        let error = scoper
            .destination(&r2, grant("base/external_services/redis/c/"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, WorkerCredentialError::Unscopable { .. }),
            "{error}"
        );

        r2.worker_credentials = s3_sources::WORKER_CREDENTIALS_ALLOW_FULL.to_string();
        let full = scoper
            .destination(&r2, grant("base/external_services/redis/c/"))
            .await
            .unwrap();
        assert_eq!(full.destination.access_key_id, "source-key");
        assert!(matches!(full.scope, WorkerCredentialScope::Full { .. }));
        let warning = full.warning("offsite", 2).unwrap();
        assert!(warning.contains("full credential"), "{warning}");
        assert!(warning.contains("node 2"), "{warning}");
    }

    #[tokio::test]
    async fn an_unreachable_sts_is_retryable_and_never_falls_back() {
        let encryption = encryption();
        let scoper = WorkerCredentialScoper::new(encryption.clone());
        // A port nothing listens on.
        let mut store = source(Some("http://127.0.0.1:9"), "base");
        store.access_key_id = encryption.encrypt_string("source-key").unwrap();
        store.secret_key = encryption.encrypt_string("source-secret").unwrap();
        store.worker_credentials = s3_sources::WORKER_CREDENTIALS_ALLOW_FULL.to_string();
        let error = scoper
            .destination(&store, grant("base/external_services/redis/c/"))
            .await
            .unwrap_err();
        assert!(error.is_retryable(), "{error}");
        assert!(
            matches!(error, WorkerCredentialError::Transient { .. }),
            "{error}"
        );
    }
}

/// The scoper against a real S3-compatible store (RustFS) with an STS: the
/// credential it mints for one service's prefix cannot list, read, write or
/// delete another's, cannot delete its own objects, and a read credential
/// cannot write. Skips without Docker.
#[cfg(test)]
#[cfg(feature = "docker-tests")]
mod docker_tests {
    use super::*;
    use crate::externalsvc::S3Credentials;
    use aws_sdk_s3::primitives::ByteStream;
    use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, InspectContainerOptions, RemoveContainerOptionsBuilder,
        StartContainerOptions,
    };
    use bollard::Docker;
    use std::collections::HashMap;

    const RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0";
    const RUSTFS_KEY: &str = "rustfsadmin";
    const BUCKET: &str = "worker-backups";
    const BASE: &str = "base";
    const SERVICE_A: &str = "base/external_services/redis/a/";
    const SERVICE_B: &str = "base/external_services/redis/b/";

    /// Removes the one container this test created.
    struct Container {
        docker: Docker,
        name: String,
    }

    impl Container {
        async fn remove(self) {
            let _ = self
                .docker
                .remove_container(
                    &self.name,
                    Some(
                        RemoveContainerOptionsBuilder::new()
                            .force(true)
                            .v(true)
                            .build(),
                    ),
                )
                .await;
        }
    }

    async fn rustfs(docker: &Docker) -> Option<(Container, u16)> {
        if docker.inspect_image(RUSTFS_IMAGE).await.is_err() {
            eprintln!("{RUSTFS_IMAGE} is not present locally, skipping");
            return None;
        }
        let name = format!(
            "temps-test-worker-s3-scope-{}",
            uuid::Uuid::new_v4().simple()
        );
        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(RUSTFS_IMAGE.to_string()),
                    env: Some(vec![
                        format!("RUSTFS_ACCESS_KEY={RUSTFS_KEY}"),
                        format!("RUSTFS_SECRET_KEY={RUSTFS_KEY}"),
                    ]),
                    host_config: Some(HostConfig {
                        port_bindings: Some(HashMap::from([(
                            "9000/tcp".to_string(),
                            Some(vec![PortBinding {
                                host_ip: Some("127.0.0.1".to_string()),
                                host_port: Some(String::new()),
                            }]),
                        )])),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let container = Container {
            docker: docker.clone(),
            name: name.clone(),
        };
        docker
            .start_container(&name, None::<StartContainerOptions>)
            .await
            .unwrap();
        let port = docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await
            .unwrap()
            .network_settings
            .and_then(|settings| settings.ports)
            .and_then(|ports| ports.get("9000/tcp").cloned().flatten())
            .and_then(|bindings| bindings.first().and_then(|b| b.host_port.clone()))
            .and_then(|port| port.parse().ok())
            .unwrap();
        Some((container, port))
    }

    fn client_credentials(destination: &WorkerS3Destination) -> S3Credentials {
        S3Credentials {
            access_key_id: destination.access_key_id.clone(),
            secret_key: destination.secret_access_key.clone(),
            session_token: destination.session_token.clone(),
            region: destination.region.clone(),
            endpoint: destination.endpoint.clone(),
            bucket_name: destination.bucket.clone(),
            bucket_path: String::new(),
            force_path_style: true,
        }
    }

    async fn put(client: &aws_sdk_s3::Client, key: &str) -> Result<(), String> {
        client
            .put_object()
            .bucket(BUCKET)
            .key(key)
            .body(ByteStream::from_static(b"dump"))
            .send()
            .await
            .map(|_| ())
            .map_err(|error| format!("{:?}", error.into_service_error()))
    }

    async fn get(client: &aws_sdk_s3::Client, key: &str) -> Result<(), String> {
        client
            .get_object()
            .bucket(BUCKET)
            .key(key)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| format!("{:?}", error.into_service_error()))
    }

    async fn list(client: &aws_sdk_s3::Client, prefix: &str) -> Result<usize, String> {
        client
            .list_objects_v2()
            .bucket(BUCKET)
            .prefix(prefix)
            .send()
            .await
            .map(|out| out.contents().len())
            .map_err(|error| format!("{:?}", error.into_service_error()))
    }

    async fn delete(client: &aws_sdk_s3::Client, key: &str) -> Result<(), String> {
        client
            .delete_object()
            .bucket(BUCKET)
            .key(key)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| format!("{:?}", error.into_service_error()))
    }

    fn denied(result: Result<impl std::fmt::Debug, String>, what: &str) {
        match result {
            Ok(value) => panic!("{what} should be denied, got {value:?}"),
            Err(error) => assert!(error.contains("AccessDenied"), "{what}: {error}"),
        }
    }

    #[tokio::test]
    async fn a_scoped_worker_credential_reaches_only_its_own_prefix() {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable, skipping");
            return;
        }
        let Some((container, port)) = rustfs(&docker).await else {
            return;
        };
        let endpoint = format!("http://127.0.0.1:{port}");
        let admin = S3Credentials {
            access_key_id: RUSTFS_KEY.to_string(),
            secret_key: RUSTFS_KEY.to_string(),
            session_token: None,
            region: DEFAULT_REGION.to_string(),
            endpoint: Some(endpoint.clone()),
            bucket_name: BUCKET.to_string(),
            bucket_path: String::new(),
            force_path_style: true,
        }
        .build_s3_client()
        .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            match admin.create_bucket().bucket(BUCKET).send().await {
                Ok(_) => break,
                Err(error) if tokio::time::Instant::now() > deadline => {
                    container.remove().await;
                    panic!("RustFS never accepted the bucket: {error:?}")
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        let other_key = format!("{SERVICE_B}2026/11/08/x/dump.rdb.gz");
        put(&admin, &other_key).await.unwrap();

        let encryption = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        let now = Utc::now();
        let source = s3_sources::Model {
            id: 1,
            name: "rustfs".to_string(),
            bucket_name: BUCKET.to_string(),
            region: DEFAULT_REGION.to_string(),
            endpoint: Some(endpoint.clone()),
            bucket_path: BASE.to_string(),
            access_key_id: encryption.encrypt_string(RUSTFS_KEY).unwrap(),
            secret_key: encryption.encrypt_string(RUSTFS_KEY).unwrap(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            worker_credentials: s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
            created_at: now,
            updated_at: now,
            backing_service_id: None,
        };
        let scoper = WorkerCredentialScoper::new(encryption);

        let write = scoper
            .destination(
                &source,
                WorkerS3Grant::job(7, 2, SERVICE_A, ScopedS3Access::Write, "backup"),
            )
            .await
            .unwrap();
        assert!(matches!(write.scope, WorkerCredentialScope::Scoped { .. }));
        assert_ne!(write.destination.access_key_id, RUSTFS_KEY);
        assert!(write.destination.session_token.is_some());
        let worker = client_credentials(&write.destination)
            .build_s3_client()
            .await;
        let own_key = format!("{SERVICE_A}2026/11/08/y/dump.rdb.gz");

        // Its own prefix: write, read back, list.
        put(&worker, &own_key).await.unwrap();
        get(&worker, &own_key).await.unwrap();
        assert_eq!(list(&worker, SERVICE_A).await.unwrap(), 1);
        // Another service's prefix, the source's base and the whole bucket.
        denied(list(&worker, SERVICE_B).await, "listing another service");
        denied(list(&worker, BASE).await, "listing the source's base");
        denied(list(&worker, "").await, "listing the bucket");
        denied(get(&worker, &other_key).await, "reading another service");
        denied(
            put(&worker, &format!("{SERVICE_B}planted")).await,
            "writing another service",
        );
        denied(
            delete(&worker, &other_key).await,
            "deleting another service",
        );
        // No delete at all, even of its own objects: retention runs on the plane.
        denied(delete(&worker, &own_key).await, "deleting its own backup");

        let read = scoper
            .destination(
                &source,
                WorkerS3Grant::job(7, 2, SERVICE_A, ScopedS3Access::Read, "restore"),
            )
            .await
            .unwrap();
        let restorer = client_credentials(&read.destination)
            .build_s3_client()
            .await;
        get(&restorer, &own_key).await.unwrap();
        assert_eq!(list(&restorer, SERVICE_A).await.unwrap(), 1);
        denied(
            put(&restorer, &format!("{SERVICE_A}overwrite")).await,
            "a restore writing",
        );
        denied(
            get(&restorer, &other_key).await,
            "a restore reading another service",
        );

        // Both objects are still there, untouched.
        get(&admin, &other_key).await.unwrap();
        get(&admin, &own_key).await.unwrap();
        container.remove().await;
    }
}
