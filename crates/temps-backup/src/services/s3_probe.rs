// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reachability checks for an S3 destination before it is saved or when an
//! operator tests it.
//!
//! An S3 destination is operator input: a mistyped endpoint, a hostname that
//! only resolves on another machine, or a key for the wrong account. When the
//! check fails the operator needs to know which of those it was, so every
//! failure is classified ([`S3ProbeFailureKind`]) and carries the endpoint,
//! the bucket and the underlying cause.
//!
//! `Display` on the SDK's `SdkError` stops at "dispatch failure" or "service
//! error"; the useful part (a DNS error, a refused connection, an S3 error
//! code) is further down the error's source chain or in the parsed response,
//! which is where [`classify_s3_error`] looks. Credentials never appear in
//! the result: the response body (which can echo the access key ID and the
//! string to sign) is not included.

use std::time::Duration;

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::Client as S3Client;

/// Longest a single reachability check may take, all retries included, so a
/// host that accepts connections and never answers cannot hold the request
/// open.
pub(crate) const S3_PROBE_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Why an S3 destination could not be verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3ProbeFailureKind {
    /// The endpoint's hostname does not resolve from this server.
    DnsResolution,
    /// The host answered with a refusal: nothing listens on that port.
    ConnectionRefused,
    /// The connection or the request did not complete in time.
    Timeout,
    /// The connection failed some other way (reset, unreachable network).
    ConnectionFailed,
    /// The TLS handshake failed: wrong scheme, or an untrusted certificate.
    Tls,
    /// The endpoint URL itself is unusable.
    InvalidEndpoint,
    /// The access key or secret key was rejected.
    Authentication,
    /// The credentials are valid but may not use this bucket.
    AccessDenied,
    /// The bucket does not exist.
    BucketNotFound,
    /// The bucket name is not a valid S3 bucket name.
    InvalidBucketName,
    /// The bucket lives in another region than the one configured.
    WrongRegion,
    /// Something answered, but with a server error or a response that is not
    /// S3's.
    UpstreamError,
}

impl S3ProbeFailureKind {
    /// Stable identifier, exposed to API clients as the problem's `failure`.
    pub fn slug(self) -> &'static str {
        match self {
            Self::DnsResolution => "dns_resolution",
            Self::ConnectionRefused => "connection_refused",
            Self::Timeout => "timeout",
            Self::ConnectionFailed => "connection_failed",
            Self::Tls => "tls",
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::Authentication => "authentication",
            Self::AccessDenied => "access_denied",
            Self::BucketNotFound => "bucket_not_found",
            Self::InvalidBucketName => "invalid_bucket_name",
            Self::WrongRegion => "wrong_region",
            Self::UpstreamError => "upstream_error",
        }
    }

    /// Problem title for the failure.
    pub fn title(self) -> &'static str {
        match self {
            Self::DnsResolution => "S3 Endpoint Not Resolvable",
            Self::ConnectionRefused => "S3 Endpoint Refused Connection",
            Self::Timeout => "S3 Endpoint Timed Out",
            Self::ConnectionFailed => "S3 Endpoint Unreachable",
            Self::Tls => "S3 Endpoint TLS Error",
            Self::InvalidEndpoint => "Invalid S3 Endpoint",
            Self::Authentication => "S3 Authentication Failed",
            Self::AccessDenied => "S3 Access Denied",
            Self::BucketNotFound => "S3 Bucket Not Found",
            Self::InvalidBucketName => "Invalid S3 Bucket Name",
            Self::WrongRegion => "S3 Region Mismatch",
            Self::UpstreamError => "S3 Endpoint Error",
        }
    }

    /// What happened, in a few words.
    pub fn summary(self) -> &'static str {
        match self {
            Self::DnsResolution => "DNS lookup of the endpoint's hostname failed",
            Self::ConnectionRefused => "the connection was refused",
            Self::Timeout => "the endpoint did not answer in time",
            Self::ConnectionFailed => "the connection to the endpoint failed",
            Self::Tls => "the TLS handshake failed",
            Self::InvalidEndpoint => "the endpoint URL is not valid",
            Self::Authentication => "the access key or secret key was rejected",
            Self::AccessDenied => "access to the bucket was denied",
            Self::BucketNotFound => "the bucket does not exist",
            Self::InvalidBucketName => "the bucket name is not valid",
            Self::WrongRegion => "the bucket is in a different region",
            Self::UpstreamError => "the endpoint returned an error",
        }
    }

    /// What the operator can change.
    pub fn hint(self) -> &'static str {
        match self {
            Self::DnsResolution => {
                "Check the endpoint URL, and that its hostname resolves from the Temps server \
                 (a name such as host.docker.internal only resolves inside containers)"
            }
            Self::ConnectionRefused => {
                "Check the endpoint's port, and that the S3 service is running and reachable \
                 from the Temps server"
            }
            Self::Timeout => {
                "Check that the endpoint is reachable from the Temps server and that no \
                 firewall drops the connection"
            }
            Self::ConnectionFailed => {
                "Check the endpoint URL and the network path from the Temps server"
            }
            Self::Tls => {
                "Check the URL scheme (http:// or https://) matches the service, and that \
                 its certificate is valid for the hostname"
            }
            Self::InvalidEndpoint => "Use a full URL such as https://s3.example.com",
            Self::Authentication => {
                "Check the access key ID and secret key, and that they belong to this service"
            }
            Self::AccessDenied => "Grant the credentials list, read and write access to the bucket",
            Self::BucketNotFound => "Create the bucket, or check its name",
            Self::InvalidBucketName => {
                "Use 3-63 lowercase letters, digits, dots and hyphens, starting and ending \
                 with a letter or digit"
            }
            Self::WrongRegion => "Set the region the bucket was created in",
            Self::UpstreamError => {
                "Check that the endpoint is an S3-compatible API and that the service is healthy"
            }
        }
    }

    /// Whether the operator's input is what must change (`400`), rather than
    /// the reachability or health of the service behind it (`502`).
    pub fn is_configuration_error(self) -> bool {
        match self {
            Self::DnsResolution
            | Self::Tls
            | Self::InvalidEndpoint
            | Self::Authentication
            | Self::AccessDenied
            | Self::BucketNotFound
            | Self::InvalidBucketName
            | Self::WrongRegion => true,
            Self::ConnectionRefused
            | Self::Timeout
            | Self::ConnectionFailed
            | Self::UpstreamError => false,
        }
    }
}

/// A classified failure and the technical cause behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3ProbeFailure {
    pub kind: S3ProbeFailureKind,
    /// The error chain, or the S3 status, code and message. Never the
    /// response body or any credential.
    pub cause: String,
}

/// Classify a failed S3 call.
pub fn classify_s3_error<E>(error: &SdkError<E, HttpResponse>) -> S3ProbeFailure
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    match error {
        SdkError::ServiceError(service) => {
            let status = service.raw().status().as_u16();
            let code = service.err().code();
            let kind = match code {
                Some(
                    "InvalidAccessKeyId"
                    | "SignatureDoesNotMatch"
                    | "InvalidToken"
                    | "ExpiredToken"
                    | "InvalidSecurity"
                    | "TokenRefreshRequired",
                ) => S3ProbeFailureKind::Authentication,
                Some(
                    "AuthorizationHeaderMalformed"
                    | "PermanentRedirect"
                    | "IllegalLocationConstraintException",
                ) => S3ProbeFailureKind::WrongRegion,
                Some("AccessDenied" | "AllAccessDisabled" | "AccountProblem") => {
                    S3ProbeFailureKind::AccessDenied
                }
                Some("NoSuchBucket") => S3ProbeFailureKind::BucketNotFound,
                Some("InvalidBucketName") => S3ProbeFailureKind::InvalidBucketName,
                _ => match status {
                    301 => S3ProbeFailureKind::WrongRegion,
                    401 => S3ProbeFailureKind::Authentication,
                    403 => S3ProbeFailureKind::AccessDenied,
                    404 => S3ProbeFailureKind::BucketNotFound,
                    _ => S3ProbeFailureKind::UpstreamError,
                },
            };
            let mut cause = format!("HTTP {status}");
            if let Some(code) = code {
                cause.push_str(&format!(", code {code}"));
            }
            if let Some(message) = service.err().message() {
                cause.push_str(&format!(": {message}"));
            }
            S3ProbeFailure { kind, cause }
        }
        SdkError::TimeoutError(_) => S3ProbeFailure {
            kind: S3ProbeFailureKind::Timeout,
            cause: error_chain(error),
        },
        SdkError::DispatchFailure(dispatch) => {
            let cause = error_chain(error);
            let kind = if dispatch.is_timeout() {
                S3ProbeFailureKind::Timeout
            } else {
                classify_transport(error, &cause)
            };
            S3ProbeFailure { kind, cause }
        }
        SdkError::ConstructionFailure(_) => S3ProbeFailure {
            kind: S3ProbeFailureKind::InvalidEndpoint,
            cause: error_chain(error),
        },
        // A response arrived that is not a parseable S3 response: something
        // other than an S3 API answers at the endpoint.
        _ => S3ProbeFailure {
            kind: S3ProbeFailureKind::UpstreamError,
            cause: error_chain(error),
        },
    }
}

/// The messages of `error` and its sources, joined, without repeats (each
/// layer of the HTTP stack often repeats the one beneath it).
fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = Some(error);
    while let Some(layer) = current {
        let message = layer.to_string();
        if !message.is_empty() && !parts.iter().any(|part| part.contains(&message)) {
            parts.push(message);
        }
        current = layer.source();
    }
    parts.join(": ")
}

/// A transport failure, from the `std::io::Error` in the chain when there is
/// one, otherwise from the resolver's and TLS stack's messages.
fn classify_transport(
    error: &(dyn std::error::Error + 'static),
    chain: &str,
) -> S3ProbeFailureKind {
    let mut current = Some(error);
    while let Some(layer) = current {
        if let Some(io) = layer.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => {
                    return S3ProbeFailureKind::ConnectionRefused
                }
                std::io::ErrorKind::TimedOut => return S3ProbeFailureKind::Timeout,
                _ => {}
            }
        }
        current = layer.source();
    }
    let lower = chain.to_ascii_lowercase();
    let mentions = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if mentions(&[
        "dns error",
        "failed to lookup address",
        "nodename nor servname",
        "name or service not known",
        "no such host",
        "temporary failure in name resolution",
        "no address associated with hostname",
    ]) {
        S3ProbeFailureKind::DnsResolution
    } else if mentions(&["connection refused"]) {
        S3ProbeFailureKind::ConnectionRefused
    } else if mentions(&[
        "tls",
        "ssl",
        "certificate",
        "handshake",
        "invalidcontenttype",
        "corrupt message",
    ]) {
        S3ProbeFailureKind::Tls
    } else if mentions(&["timed out", "timeout"]) {
        S3ProbeFailureKind::Timeout
    } else {
        S3ProbeFailureKind::ConnectionFailed
    }
}

/// Where the destination points, for messages: the custom endpoint without
/// any user information, or AWS S3 in the configured region.
pub fn display_endpoint(endpoint: Option<&str>, region: &str) -> String {
    match endpoint
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
    {
        Some(endpoint) => {
            let url = endpoint_url(endpoint);
            match url::Url::parse(&url) {
                Ok(mut parsed) => {
                    let _ = parsed.set_username("");
                    let _ = parsed.set_password(None);
                    parsed.to_string().trim_end_matches('/').to_string()
                }
                // Not a URL: keep only what precedes any `user@` part.
                Err(_) => url.rsplit('@').next().unwrap_or_default().to_string(),
            }
        }
        None => format!("AWS S3 (region {region})"),
    }
}

/// The endpoint as the SDK receives it: a bare `host:port` means plain HTTP,
/// matching what an S3-compatible service on a private network expects.
pub fn endpoint_url(endpoint: &str) -> String {
    if endpoint.starts_with("http") {
        endpoint.to_string()
    } else {
        format!("http://{}", endpoint)
    }
}

/// What a reachability check does when the bucket does not exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingBucket {
    /// Create it: the destination is being saved and will need it.
    Create,
    /// Report success: the credentials and endpoint work, and saving the
    /// destination will create it.
    Accept,
    /// Report it: an existing destination must already have its bucket.
    Fail,
}

/// Verify that `bucket` is reachable through `client`, classifying any
/// failure. `on_missing` decides what a missing bucket means.
pub async fn check_bucket(
    client: &S3Client,
    bucket: &str,
    on_missing: MissingBucket,
) -> Result<(), S3ProbeFailure> {
    let failure = match client
        .list_objects_v2()
        .bucket(bucket)
        .max_keys(1)
        .send()
        .await
    {
        Ok(_) => return Ok(()),
        Err(error) => classify_s3_error(&error),
    };
    if failure.kind != S3ProbeFailureKind::BucketNotFound {
        return Err(failure);
    }
    match on_missing {
        MissingBucket::Accept => Ok(()),
        MissingBucket::Fail => Err(failure),
        MissingBucket::Create => client
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| classify_s3_error(&error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn client(endpoint: &str, operation_timeout: Duration) -> S3Client {
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .force_path_style(true)
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test-access-key",
                "test-secret-key",
                None,
                None,
                "test",
            ))
            .http_client(crate::engines::v2_common::bundled_roots_http_client())
            .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
            .timeout_config(
                aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                    .operation_timeout(operation_timeout)
                    .build(),
            )
            .endpoint_url(endpoint)
            .build();
        S3Client::from_conf(config)
    }

    /// A one-request HTTP server answering every request with `response`.
    async fn serve(response: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{address}")
    }

    fn s3_error(status: &str, code: &str, message: &str) -> String {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{message}</Message><AWSAccessKeyId>test-access-key</AWSAccessKeyId><RequestId>r1</RequestId></Error>"
        );
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn probe(endpoint: &str, on_missing: MissingBucket) -> Result<(), S3ProbeFailure> {
        check_bucket(
            &client(endpoint, Duration::from_secs(10)),
            "probe",
            on_missing,
        )
        .await
    }

    #[tokio::test]
    async fn an_unresolvable_host_is_a_dns_failure() {
        let failure = probe("http://temps-s3-probe.invalid:9000", MissingBucket::Fail)
            .await
            .unwrap_err();
        assert_eq!(
            failure.kind,
            S3ProbeFailureKind::DnsResolution,
            "{failure:?}"
        );
        assert!(failure.cause.contains("dns"), "{failure:?}");
    }

    #[tokio::test]
    async fn a_closed_port_is_a_refused_connection() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let failure = probe(&format!("http://127.0.0.1:{port}"), MissingBucket::Fail)
            .await
            .unwrap_err();
        assert_eq!(
            failure.kind,
            S3ProbeFailureKind::ConnectionRefused,
            "{failure:?}"
        );
    }

    #[tokio::test]
    async fn https_against_a_plain_http_port_is_a_tls_failure() {
        let endpoint =
            serve("HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_string()).await;
        let failure = probe(
            &endpoint.replace("http://", "https://"),
            MissingBucket::Fail,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.kind, S3ProbeFailureKind::Tls, "{failure:?}");
    }

    #[tokio::test]
    async fn a_host_that_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let mut sockets = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                sockets.push(socket);
            }
        });
        let failure = check_bucket(
            &client(&format!("http://{address}"), Duration::from_millis(500)),
            "probe",
            MissingBucket::Fail,
        )
        .await
        .unwrap_err();
        held.abort();
        assert_eq!(failure.kind, S3ProbeFailureKind::Timeout, "{failure:?}");
    }

    #[tokio::test]
    async fn s3_error_codes_are_classified_without_echoing_the_body() {
        for (status, code, kind) in [
            (
                "403 Forbidden",
                "InvalidAccessKeyId",
                S3ProbeFailureKind::Authentication,
            ),
            (
                "403 Forbidden",
                "SignatureDoesNotMatch",
                S3ProbeFailureKind::Authentication,
            ),
            (
                "403 Forbidden",
                "AccessDenied",
                S3ProbeFailureKind::AccessDenied,
            ),
            (
                "400 Bad Request",
                "AuthorizationHeaderMalformed",
                S3ProbeFailureKind::WrongRegion,
            ),
            (
                "400 Bad Request",
                "InvalidBucketName",
                S3ProbeFailureKind::InvalidBucketName,
            ),
            (
                "503 Service Unavailable",
                "SlowDown",
                S3ProbeFailureKind::UpstreamError,
            ),
        ] {
            let endpoint = serve(s3_error(status, code, "Rejected by the test server")).await;
            let failure = probe(&endpoint, MissingBucket::Fail).await.unwrap_err();
            assert_eq!(failure.kind, kind, "{code}: {failure:?}");
            assert!(failure.cause.contains(code), "{failure:?}");
            assert!(
                failure.cause.contains("Rejected by the test server"),
                "{failure:?}"
            );
            assert!(!failure.cause.contains("test-access-key"), "{failure:?}");
        }
    }

    #[tokio::test]
    async fn a_missing_bucket_follows_the_callers_policy() {
        let missing = s3_error(
            "404 Not Found",
            "NoSuchBucket",
            "The specified bucket does not exist",
        );
        let endpoint = serve(missing).await;
        assert_eq!(probe(&endpoint, MissingBucket::Accept).await, Ok(()));
        assert_eq!(
            probe(&endpoint, MissingBucket::Fail)
                .await
                .unwrap_err()
                .kind,
            S3ProbeFailureKind::BucketNotFound
        );
        // Creating it fails here too (the server always answers 404), and
        // that failure is what is reported.
        assert_eq!(
            probe(&endpoint, MissingBucket::Create)
                .await
                .unwrap_err()
                .kind,
            S3ProbeFailureKind::BucketNotFound
        );
    }

    #[test]
    fn endpoints_are_displayed_without_credentials() {
        assert_eq!(
            display_endpoint(
                Some("https://user:secret@s3.example.test:9000/"),
                "us-east-1"
            ),
            "https://s3.example.test:9000"
        );
        assert_eq!(
            display_endpoint(Some("minio.internal.test:9000"), "us-east-1"),
            "http://minio.internal.test:9000"
        );
        assert_eq!(
            display_endpoint(None, "eu-west-1"),
            "AWS S3 (region eu-west-1)"
        );
        assert_eq!(
            display_endpoint(Some("  "), "eu-west-1"),
            "AWS S3 (region eu-west-1)"
        );
    }

    #[test]
    fn configuration_errors_are_the_operators_to_fix() {
        assert!(S3ProbeFailureKind::DnsResolution.is_configuration_error());
        assert!(S3ProbeFailureKind::Authentication.is_configuration_error());
        assert!(!S3ProbeFailureKind::ConnectionRefused.is_configuration_error());
        assert!(!S3ProbeFailureKind::UpstreamError.is_configuration_error());
    }
}
