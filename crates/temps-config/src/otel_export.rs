// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Configuration for exporting the Temps server's **own** traces over OTLP.
//!
//! This module is pure parsing and precedence logic: it has no OpenTelemetry
//! dependency and performs no I/O except [`load_file`]. The exporter itself is
//! built in `temps-cli` from the [`OtelExportConfig`] resolved here.
//!
//! # Precedence
//!
//! For every setting, the first layer that provides a valid value wins:
//!
//! 1. CLI flag (`temps serve --otel-*`, plus `TEMPS_OTEL_EXPORT`, the variable
//!    clap binds to `--otel-export`)
//! 2. Environment: the standard `OTEL_*` variables
//! 3. Config file: `<data-dir>/otel-export.yaml`
//! 4. Built-in default (export **disabled**)
//!
//! A value that fails to parse never aborts startup. It is reported as an
//! [`OtelConfigIssue`] and the next layer is consulted, so a typo in an
//! observability setting cannot take the server down. The one exception to
//! "fall through" is an unusable endpoint: with nowhere valid to send spans,
//! export is switched off rather than silently redirected to a default.
//!
//! # Why this is not a database setting
//!
//! The tracer provider has to exist before the database connection does (the
//! subscriber is installed first thing, and startup spans are the interesting
//! ones), so this is bootstrap configuration in the same sense as
//! `TEMPS_DATABASE_URL`.
//!
//! # Secrets
//!
//! OTLP headers usually carry an API key. They are accepted from the
//! environment only (never a flag, which would leak into `ps`; never the
//! config file, which is not encrypted at rest), and [`OtlpHeaders`] redacts
//! every value in `Debug`/`Display`. Issue messages never echo header values.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// `service.name` when nothing else sets it.
pub const DEFAULT_SERVICE_NAME: &str = "temps";
/// File name looked up inside the Temps data directory.
pub const CONFIG_FILE_NAME: &str = "otel-export.yaml";
/// Default OTLP/gRPC endpoint (the OpenTelemetry-wide default).
pub const DEFAULT_GRPC_ENDPOINT: &str = "http://localhost:4317";
/// Default OTLP/HTTP base endpoint (the OpenTelemetry-wide default).
pub const DEFAULT_HTTP_ENDPOINT: &str = "http://localhost:4318";
/// Path appended to a *base* OTLP/HTTP endpoint, per the OTLP specification.
pub const HTTP_TRACES_PATH: &str = "/v1/traces";
/// Upper bound on a single export request when nothing configures it.
pub const DEFAULT_EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

const MIN_EXPORT_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_EXPORT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_HEADERS: usize = 32;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_RESOURCE_ATTRIBUTES: usize = 64;
const REDACTED: &str = "***";

/// OTLP transport. `http/json` is deliberately unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OtlpProtocol {
    #[serde(rename = "grpc")]
    Grpc,
    /// The OpenTelemetry-specified default.
    #[default]
    #[serde(rename = "http/protobuf")]
    HttpProtobuf,
}

impl OtlpProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            OtlpProtocol::Grpc => "grpc",
            OtlpProtocol::HttpProtobuf => "http/protobuf",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "grpc" => Ok(OtlpProtocol::Grpc),
            "http/protobuf" => Ok(OtlpProtocol::HttpProtobuf),
            other => Err(format!(
                "'{other}' is not a supported OTLP protocol (expected 'grpc' or 'http/protobuf')"
            )),
        }
    }
}

impl fmt::Display for OtlpProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Head-based sampling decision, mirroring the `OTEL_TRACES_SAMPLER` values.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TracesSampler {
    AlwaysOn,
    AlwaysOff,
    TraceIdRatio(f64),
    /// The OpenTelemetry-specified default.
    #[default]
    ParentBasedAlwaysOn,
    ParentBasedAlwaysOff,
    ParentBasedTraceIdRatio(f64),
}

impl fmt::Display for TracesSampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TracesSampler::AlwaysOn => f.write_str("always_on"),
            TracesSampler::AlwaysOff => f.write_str("always_off"),
            TracesSampler::TraceIdRatio(r) => write!(f, "traceidratio({r})"),
            TracesSampler::ParentBasedAlwaysOn => f.write_str("parentbased_always_on"),
            TracesSampler::ParentBasedAlwaysOff => f.write_str("parentbased_always_off"),
            TracesSampler::ParentBasedTraceIdRatio(r) => {
                write!(f, "parentbased_traceidratio({r})")
            }
        }
    }
}

/// OTLP request headers. Names are public, values are not: `Debug` and
/// `Display` print `name=***`. Use [`OtlpHeaders::iter`] to hand the real
/// values to the exporter.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct OtlpHeaders(Vec<(String, String)>);

impl OtlpHeaders {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The real `(name, value)` pairs. Names are lowercase.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Header names only, for logs.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    #[cfg(test)]
    fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

impl fmt::Display for OtlpHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;
        for (i, (name, _)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{name}={REDACTED}")?;
        }
        f.write_str("]")
    }
}

impl fmt::Debug for OtlpHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OtlpHeaders{self}")
    }
}

/// Fully resolved export settings.
#[derive(Clone, PartialEq)]
pub struct OtelExportConfig {
    pub enabled: bool,
    pub protocol: OtlpProtocol,
    /// The final URL the exporter posts to (for OTLP/HTTP this already ends
    /// in `/v1/traces` unless the signal-specific variable supplied it).
    /// May contain credentials; log it only through [`redact_endpoint`].
    pub endpoint: String,
    pub headers: OtlpHeaders,
    /// Value of the `service.name` resource attribute.
    pub service_name: String,
    /// Operator-supplied resource attributes, excluding `service.name`.
    pub resource_attributes: BTreeMap<String, String>,
    pub sampler: TracesSampler,
    pub export_timeout: Duration,
}

impl Default for OtelExportConfig {
    /// Export disabled; every other field is the specification default.
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: OtlpProtocol::default(),
            endpoint: format!("{DEFAULT_HTTP_ENDPOINT}{HTTP_TRACES_PATH}"),
            headers: OtlpHeaders::default(),
            service_name: DEFAULT_SERVICE_NAME.to_string(),
            resource_attributes: BTreeMap::new(),
            sampler: TracesSampler::default(),
            export_timeout: DEFAULT_EXPORT_TIMEOUT,
        }
    }
}

impl OtelExportConfig {
    /// One-line description that is safe to log: endpoint credentials and
    /// query are stripped and header values are masked.
    pub fn redacted_summary(&self) -> String {
        format!(
            "protocol={} endpoint={} headers={} service.name={} sampler={} timeout={:?} \
             resource_attributes={}",
            self.protocol,
            redact_endpoint(&self.endpoint),
            self.headers,
            self.service_name,
            self.sampler,
            self.export_timeout,
            self.resource_attributes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

impl fmt::Debug for OtelExportConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtelExportConfig")
            .field("enabled", &self.enabled)
            .field("protocol", &self.protocol)
            .field("endpoint", &redact_endpoint(&self.endpoint))
            .field("headers", &self.headers)
            .field("service_name", &self.service_name)
            .field(
                "resource_attribute_keys",
                &self.resource_attributes.keys().collect::<Vec<_>>(),
            )
            .field("sampler", &self.sampler)
            .field("export_timeout", &self.export_timeout)
            .finish()
    }
}

/// Values supplied on the command line (or through the `TEMPS_OTEL_*`
/// variables clap binds to those flags). Everything is a string so that a
/// malformed value becomes an [`OtelConfigIssue`] instead of a clap abort.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OtelCliOverrides {
    /// `true`/`false`/`1`/`0`/`yes`/`no`/`on`/`off`.
    pub enabled: Option<String>,
    /// A *base* endpoint, like `OTEL_EXPORTER_OTLP_ENDPOINT`.
    pub endpoint: Option<String>,
    pub protocol: Option<String>,
    pub service_name: Option<String>,
    /// Fraction of traces to keep, `0.0..=1.0`.
    pub sample_ratio: Option<String>,
}

/// The optional `<data-dir>/otel-export.yaml` layer.
///
/// There is deliberately no `headers` key: headers are credentials, and this
/// file is stored unencrypted. Supply them with
/// `OTEL_EXPORTER_OTLP_HEADERS`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OtelExportFile {
    pub enabled: Option<bool>,
    /// A *base* endpoint, like `OTEL_EXPORTER_OTLP_ENDPOINT`.
    pub endpoint: Option<String>,
    /// `grpc` or `http/protobuf`.
    pub protocol: Option<String>,
    pub service_name: Option<String>,
    /// Fraction of traces to keep, `0.0..=1.0`.
    pub sample_ratio: Option<f64>,
    pub export_timeout_secs: Option<u64>,
    #[serde(default)]
    pub resource_attributes: BTreeMap<String, String>,
}

/// A configuration problem that was worked around. None of these stop the
/// server; the caller logs each at `warn` once the subscriber is installed.
/// No variant carries a header value.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum OtelConfigIssue {
    #[error(
        "OTLP endpoint from {origin} ({endpoint}) is unusable: {reason}; trace export is disabled"
    )]
    InvalidEndpoint {
        origin: String,
        /// Already redacted.
        endpoint: String,
        reason: String,
    },
    #[error(
        "{origin} is not a valid OTLP protocol: {reason}; trying the next configuration layer"
    )]
    InvalidProtocol { origin: String, reason: String },
    #[error("{origin} is not a valid boolean ('{value}'); ignoring it")]
    InvalidBool { origin: String, value: String },
    #[error("{origin} is not a valid sampler ({reason}); trying the next configuration layer")]
    InvalidSampler { origin: String, reason: String },
    #[error(
        "{origin} is not a valid sampling ratio ('{value}', expected a number from 0 to 1); \
         using {fallback} instead"
    )]
    InvalidSampleRatio {
        origin: String,
        value: String,
        fallback: String,
    },
    #[error(
        "{origin} is not a valid export timeout ({reason}); trying the next configuration layer"
    )]
    InvalidTimeout { origin: String, reason: String },
    #[error("{origin} entry #{index} is ignored: {reason}")]
    InvalidListEntry {
        origin: String,
        index: usize,
        reason: String,
    },
    #[error("{origin} lists more than {max} entries; the extra entries are ignored")]
    TooManyEntries { origin: String, max: usize },
    #[error("OTLP config file {path} was ignored: {reason}")]
    InvalidConfigFile { path: String, reason: String },
}

/// What [`resolve`] produces: the settings plus everything that had to be
/// worked around to get there.
#[derive(Debug, Clone, PartialEq)]
pub struct OtelExportResolution {
    pub config: OtelExportConfig,
    pub issues: Vec<OtelConfigIssue>,
}

/// Path of the config file inside `data_dir`.
pub fn config_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE_NAME)
}

/// Read `<data_dir>/otel-export.yaml`. A missing file is not an error
/// (`Ok(None)`); an unreadable or malformed one is returned as an issue so
/// the caller can warn and carry on with the remaining layers.
pub fn load_file(data_dir: &Path) -> Result<Option<OtelExportFile>, OtelConfigIssue> {
    let path = config_file_path(data_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(OtelConfigIssue::InvalidConfigFile {
                path: path.display().to_string(),
                reason: format!("cannot read file: {e}"),
            })
        }
    };
    parse_file(&raw)
        .map(Some)
        .map_err(|reason| OtelConfigIssue::InvalidConfigFile {
            path: path.display().to_string(),
            reason,
        })
}

fn parse_file(raw: &str) -> Result<OtelExportFile, String> {
    if raw.trim().is_empty() {
        return Ok(OtelExportFile::default());
    }
    serde_yaml::from_str(raw).map_err(|e| {
        let mut reason = format!("invalid YAML: {e}");
        if reason.contains("headers") {
            reason.push_str(
                " (OTLP headers are credentials and cannot be set here; use \
                 OTEL_EXPORTER_OTLP_HEADERS)",
            );
        }
        reason
    })
}

/// Look a variable up in the real process environment.
pub fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Resolve the effective configuration. `env` abstracts the environment so
/// precedence can be tested without touching the process.
pub fn resolve(
    cli: &OtelCliOverrides,
    file: Option<&OtelExportFile>,
    env: &dyn Fn(&str) -> Option<String>,
) -> OtelExportResolution {
    let mut issues = Vec::new();
    // Per the OpenTelemetry convention an empty variable is an unset one.
    let env = |key: &str| {
        env(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let enabled = resolve_enabled(cli, file, &env, &mut issues);
    let protocol = resolve_protocol(cli, file, &env, &mut issues);
    let endpoint = resolve_endpoint(cli, file, &env, protocol);
    let (service_name, resource_attributes) = resolve_resource(cli, file, &env, &mut issues);
    let sampler = resolve_sampler(cli, file, &env, &mut issues);
    let export_timeout = resolve_timeout(file, &env, &mut issues);
    let headers = resolve_headers(&env, &mut issues);

    let mut config = OtelExportConfig {
        enabled,
        protocol,
        endpoint: String::new(),
        headers,
        service_name,
        resource_attributes,
        sampler,
        export_timeout,
    };
    match endpoint {
        Ok(url) => config.endpoint = url,
        Err(issue) => {
            // Defaults keep the struct complete, but there is no valid place
            // to send spans, so do not guess one.
            config.endpoint = default_endpoint(protocol);
            if config.enabled {
                issues.push(issue);
                config.enabled = false;
            }
        }
    }
    OtelExportResolution { config, issues }
}

fn default_endpoint(protocol: OtlpProtocol) -> String {
    match protocol {
        OtlpProtocol::Grpc => DEFAULT_GRPC_ENDPOINT.to_string(),
        OtlpProtocol::HttpProtobuf => format!("{DEFAULT_HTTP_ENDPOINT}{HTTP_TRACES_PATH}"),
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn resolve_enabled(
    cli: &OtelCliOverrides,
    file: Option<&OtelExportFile>,
    env: &dyn Fn(&str) -> Option<String>,
    issues: &mut Vec<OtelConfigIssue>,
) -> bool {
    if let Some(raw) = cli
        .enabled
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        match parse_bool(raw) {
            Some(value) => return value,
            None => issues.push(OtelConfigIssue::InvalidBool {
                origin: "--otel-export / TEMPS_OTEL_EXPORT".to_string(),
                value: raw.to_string(),
            }),
        }
    }
    // The standard "turn it off" switches outrank the config file. Neither
    // can turn export *on*: enabling is always an explicit Temps setting.
    if let Some(raw) = env("OTEL_SDK_DISABLED") {
        match parse_bool(&raw) {
            Some(true) => return false,
            Some(false) => {}
            None => issues.push(OtelConfigIssue::InvalidBool {
                origin: "OTEL_SDK_DISABLED".to_string(),
                value: raw,
            }),
        }
    }
    if env("OTEL_TRACES_EXPORTER").is_some_and(|v| v.eq_ignore_ascii_case("none")) {
        return false;
    }
    file.and_then(|f| f.enabled).unwrap_or(false)
}

fn resolve_protocol(
    cli: &OtelCliOverrides,
    file: Option<&OtelExportFile>,
    env: &dyn Fn(&str) -> Option<String>,
    issues: &mut Vec<OtelConfigIssue>,
) -> OtlpProtocol {
    let candidates = [
        ("--otel-protocol", cli.protocol.clone()),
        (
            "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
            env("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL"),
        ),
        (
            "OTEL_EXPORTER_OTLP_PROTOCOL",
            env("OTEL_EXPORTER_OTLP_PROTOCOL"),
        ),
        (CONFIG_FILE_NAME, file.and_then(|f| f.protocol.clone())),
    ];
    for (origin, raw) in candidates {
        let Some(raw) = raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) else {
            continue;
        };
        match OtlpProtocol::parse(&raw) {
            Ok(protocol) => return protocol,
            Err(reason) => issues.push(OtelConfigIssue::InvalidProtocol {
                origin: origin.to_string(),
                reason,
            }),
        }
    }
    OtlpProtocol::default()
}

/// Returns the final endpoint URL, or the issue explaining why the chosen
/// layer's value cannot be used.
fn resolve_endpoint(
    cli: &OtelCliOverrides,
    file: Option<&OtelExportFile>,
    env: &dyn Fn(&str) -> Option<String>,
    protocol: OtlpProtocol,
) -> Result<String, OtelConfigIssue> {
    // (origin, value, is_signal_specific). A signal-specific endpoint is used
    // verbatim; a base endpoint gets `/v1/traces` appended for OTLP/HTTP.
    let candidates = [
        ("--otel-endpoint", cli.endpoint.clone(), false),
        (
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            true,
        ),
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            env("OTEL_EXPORTER_OTLP_ENDPOINT"),
            false,
        ),
        (
            CONFIG_FILE_NAME,
            file.and_then(|f| f.endpoint.clone()),
            false,
        ),
    ];
    for (origin, raw, signal_specific) in candidates {
        let Some(raw) = raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) else {
            continue;
        };
        let invalid = |reason: String| OtelConfigIssue::InvalidEndpoint {
            origin: origin.to_string(),
            endpoint: redact_endpoint(&raw),
            reason,
        };
        let mut url = Url::parse(&raw).map_err(|e| invalid(format!("not a valid URL: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(invalid(format!(
                "scheme '{}' is not supported (use http or https)",
                url.scheme()
            )));
        }
        if url.host_str().is_none() {
            return Err(invalid("the URL has no host".to_string()));
        }
        if protocol == OtlpProtocol::HttpProtobuf && !signal_specific {
            let path = url.path().trim_end_matches('/').to_string();
            url.set_path(&format!("{path}{HTTP_TRACES_PATH}"));
        }
        return Ok(url.to_string());
    }
    Ok(default_endpoint(protocol))
}

fn resolve_resource(
    cli: &OtelCliOverrides,
    file: Option<&OtelExportFile>,
    env: &dyn Fn(&str) -> Option<String>,
    issues: &mut Vec<OtelConfigIssue>,
) -> (String, BTreeMap<String, String>) {
    // File attributes first so the environment can override per key.
    let mut attributes: BTreeMap<String, String> = file
        .map(|f| f.resource_attributes.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|(k, _)| !k.trim().is_empty())
        .collect();
    if let Some(raw) = env("OTEL_RESOURCE_ATTRIBUTES") {
        for (k, v) in parse_pairs(
            "OTEL_RESOURCE_ATTRIBUTES",
            &raw,
            MAX_RESOURCE_ATTRIBUTES,
            issues,
        ) {
            attributes.insert(k, v);
        }
    }
    while attributes.len() > MAX_RESOURCE_ATTRIBUTES {
        attributes.pop_last();
    }

    let from_attributes = attributes.remove("service.name");
    let service_name = cli
        .service_name
        .clone()
        .or_else(|| env("OTEL_SERVICE_NAME"))
        .or(from_attributes)
        .or_else(|| file.and_then(|f| f.service_name.clone()))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
    (service_name, attributes)
}

fn resolve_sampler(
    cli: &OtelCliOverrides,
    file: Option<&OtelExportFile>,
    env: &dyn Fn(&str) -> Option<String>,
    issues: &mut Vec<OtelConfigIssue>,
) -> TracesSampler {
    // 1. The Temps ratio flag. Parent-based so a sampled upstream trace is
    //    never cut in half by the local ratio.
    if let Some(raw) = cli
        .sample_ratio
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        match parse_ratio(raw) {
            Some(ratio) => return TracesSampler::ParentBasedTraceIdRatio(ratio),
            None => issues.push(OtelConfigIssue::InvalidSampleRatio {
                origin: "--otel-sample-ratio".to_string(),
                value: raw.to_string(),
                fallback: "the next configuration layer".to_string(),
            }),
        }
    }
    // 2. The standard sampler variables.
    if let Some(name) = env("OTEL_TRACES_SAMPLER") {
        match parse_sampler(&name, env("OTEL_TRACES_SAMPLER_ARG"), issues) {
            Ok(sampler) => return sampler,
            Err(reason) => issues.push(OtelConfigIssue::InvalidSampler {
                origin: "OTEL_TRACES_SAMPLER".to_string(),
                reason,
            }),
        }
    }
    // 3. The config file.
    if let Some(ratio) = file.and_then(|f| f.sample_ratio) {
        if (0.0..=1.0).contains(&ratio) {
            return TracesSampler::ParentBasedTraceIdRatio(ratio);
        }
        issues.push(OtelConfigIssue::InvalidSampleRatio {
            origin: CONFIG_FILE_NAME.to_string(),
            value: ratio.to_string(),
            fallback: format!("the default ({})", TracesSampler::default()),
        });
    }
    TracesSampler::default()
}

fn parse_ratio(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|r| r.is_finite() && (0.0..=1.0).contains(r))
}

fn parse_sampler(
    name: &str,
    arg: Option<String>,
    issues: &mut Vec<OtelConfigIssue>,
) -> Result<TracesSampler, String> {
    // Per the specification the ratio defaults to 1.0 when the argument is
    // missing or invalid.
    let mut ratio = || match arg.as_deref() {
        None => 1.0,
        Some(raw) => parse_ratio(raw).unwrap_or_else(|| {
            issues.push(OtelConfigIssue::InvalidSampleRatio {
                origin: "OTEL_TRACES_SAMPLER_ARG".to_string(),
                value: raw.to_string(),
                fallback: "1.0".to_string(),
            });
            1.0
        }),
    };
    match name.trim().to_ascii_lowercase().as_str() {
        "always_on" => Ok(TracesSampler::AlwaysOn),
        "always_off" => Ok(TracesSampler::AlwaysOff),
        "traceidratio" => Ok(TracesSampler::TraceIdRatio(ratio())),
        "parentbased_always_on" => Ok(TracesSampler::ParentBasedAlwaysOn),
        "parentbased_always_off" => Ok(TracesSampler::ParentBasedAlwaysOff),
        "parentbased_traceidratio" => Ok(TracesSampler::ParentBasedTraceIdRatio(ratio())),
        other => Err(format!(
            "sampler '{other}' is not supported (expected always_on, always_off, traceidratio, \
             parentbased_always_on, parentbased_always_off or parentbased_traceidratio)"
        )),
    }
}

fn resolve_timeout(
    file: Option<&OtelExportFile>,
    env: &dyn Fn(&str) -> Option<String>,
    issues: &mut Vec<OtelConfigIssue>,
) -> Duration {
    // The OpenTelemetry variables are in milliseconds.
    for origin in [
        "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT",
        "OTEL_EXPORTER_OTLP_TIMEOUT",
    ] {
        let Some(raw) = env(origin) else { continue };
        match raw.parse::<u64>().map(Duration::from_millis) {
            Ok(d) if (MIN_EXPORT_TIMEOUT..=MAX_EXPORT_TIMEOUT).contains(&d) => return d,
            Ok(_) => issues.push(OtelConfigIssue::InvalidTimeout {
                origin: origin.to_string(),
                reason: format!(
                    "must be between {} and {} milliseconds",
                    MIN_EXPORT_TIMEOUT.as_millis(),
                    MAX_EXPORT_TIMEOUT.as_millis()
                ),
            }),
            Err(_) => issues.push(OtelConfigIssue::InvalidTimeout {
                origin: origin.to_string(),
                reason: "expected a whole number of milliseconds".to_string(),
            }),
        }
    }
    if let Some(secs) = file.and_then(|f| f.export_timeout_secs) {
        let d = Duration::from_secs(secs);
        if (MIN_EXPORT_TIMEOUT..=MAX_EXPORT_TIMEOUT).contains(&d) {
            return d;
        }
        issues.push(OtelConfigIssue::InvalidTimeout {
            origin: CONFIG_FILE_NAME.to_string(),
            reason: format!("{secs}s is outside 1s to {}s", MAX_EXPORT_TIMEOUT.as_secs()),
        });
    }
    DEFAULT_EXPORT_TIMEOUT
}

fn resolve_headers(
    env: &dyn Fn(&str) -> Option<String>,
    issues: &mut Vec<OtelConfigIssue>,
) -> OtlpHeaders {
    let (origin, raw) = match env("OTEL_EXPORTER_OTLP_TRACES_HEADERS") {
        Some(raw) => ("OTEL_EXPORTER_OTLP_TRACES_HEADERS", raw),
        None => match env("OTEL_EXPORTER_OTLP_HEADERS") {
            Some(raw) => ("OTEL_EXPORTER_OTLP_HEADERS", raw),
            None => return OtlpHeaders::default(),
        },
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in parse_pairs(origin, &raw, MAX_HEADERS, issues) {
        let name = name.to_ascii_lowercase();
        if !is_header_name(&name) {
            // Name only: the value may be the secret.
            issues.push(OtelConfigIssue::InvalidListEntry {
                origin: origin.to_string(),
                index: headers.len() + 1,
                reason: format!("'{name}' is not a valid HTTP header name"),
            });
            continue;
        }
        if value.len() > MAX_HEADER_VALUE_BYTES || value.chars().any(char::is_control) {
            issues.push(OtelConfigIssue::InvalidListEntry {
                origin: origin.to_string(),
                index: headers.len() + 1,
                reason: format!(
                    "the value of header '{name}' is too long or contains control characters"
                ),
            });
            continue;
        }
        headers.retain(|(existing, _)| *existing != name);
        headers.push((name, value));
    }
    OtlpHeaders(headers)
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// Parse `k1=v1,k2=v2` (the OpenTelemetry list format). Values are
/// percent-decoded. Issues mention the entry position, never its contents.
fn parse_pairs(
    origin: &str,
    raw: &str,
    max: usize,
    issues: &mut Vec<OtelConfigIssue>,
) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (i, entry) in raw.split(',').enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if pairs.len() >= max {
            issues.push(OtelConfigIssue::TooManyEntries {
                origin: origin.to_string(),
                max,
            });
            break;
        }
        let Some((key, value)) = entry.split_once('=') else {
            issues.push(OtelConfigIssue::InvalidListEntry {
                origin: origin.to_string(),
                index: i + 1,
                reason: "expected key=value".to_string(),
            });
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            issues.push(OtelConfigIssue::InvalidListEntry {
                origin: origin.to_string(),
                index: i + 1,
                reason: "the key is empty".to_string(),
            });
            continue;
        }
        pairs.push((key.to_string(), percent_decode(value.trim())));
    }
    pairs
}

/// Minimal percent-decoder. A malformed escape is kept literally, which is
/// what most OpenTelemetry SDKs do. Invalid UTF-8 is replaced.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Strip userinfo, query and fragment from an endpoint so it is safe to log.
/// An unparsable value is replaced wholesale rather than echoed.
pub fn redact_endpoint(endpoint: &str) -> String {
    let Ok(mut url) = Url::parse(endpoint) else {
        return "<unparsable endpoint>".to_string();
    };
    let had_credentials = !url.username().is_empty() || url.password().is_some();
    let had_query = url.query().is_some();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let mut out = url.to_string();
    if had_credentials {
        out = out.replacen("://", &format!("://{REDACTED}@"), 1);
    }
    if had_query {
        out.push_str(&format!("?{REDACTED}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn resolve_with(
        cli: &OtelCliOverrides,
        file: Option<&OtelExportFile>,
        env: &[(&str, &str)],
    ) -> OtelExportResolution {
        resolve(cli, file, &env_of(env))
    }

    fn enabled_cli() -> OtelCliOverrides {
        OtelCliOverrides {
            enabled: Some("true".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_to_disabled_with_spec_defaults() {
        let r = resolve_with(&OtelCliOverrides::default(), None, &[]);
        assert!(!r.config.enabled);
        assert!(r.issues.is_empty());
        assert_eq!(r.config.service_name, "temps");
        assert_eq!(r.config.protocol, OtlpProtocol::HttpProtobuf);
        assert_eq!(r.config.endpoint, "http://localhost:4318/v1/traces");
        assert_eq!(r.config.sampler, TracesSampler::ParentBasedAlwaysOn);
        assert_eq!(r.config.export_timeout, DEFAULT_EXPORT_TIMEOUT);
        assert!(r.config.headers.is_empty());
        assert_eq!(r.config, OtelExportConfig::default());
    }

    #[test]
    fn an_endpoint_in_the_environment_does_not_enable_export() {
        let r = resolve_with(
            &OtelCliOverrides::default(),
            None,
            &[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318")],
        );
        assert!(!r.config.enabled);
    }

    #[test]
    fn enabled_precedence_is_cli_then_env_kill_switch_then_file() {
        let file = OtelExportFile {
            enabled: Some(true),
            ..Default::default()
        };
        // file alone enables
        assert!(
            resolve_with(&OtelCliOverrides::default(), Some(&file), &[])
                .config
                .enabled
        );
        // OTEL_SDK_DISABLED outranks the file
        assert!(
            !resolve_with(
                &OtelCliOverrides::default(),
                Some(&file),
                &[("OTEL_SDK_DISABLED", "true")]
            )
            .config
            .enabled
        );
        // OTEL_TRACES_EXPORTER=none outranks the file
        assert!(
            !resolve_with(
                &OtelCliOverrides::default(),
                Some(&file),
                &[("OTEL_TRACES_EXPORTER", "none")]
            )
            .config
            .enabled
        );
        // an explicit CLI value outranks the environment, in both directions
        assert!(
            resolve_with(
                &enabled_cli(),
                Some(&file),
                &[("OTEL_SDK_DISABLED", "true")]
            )
            .config
            .enabled
        );
        let off = OtelCliOverrides {
            enabled: Some("false".to_string()),
            ..Default::default()
        };
        assert!(!resolve_with(&off, Some(&file), &[]).config.enabled);
    }

    #[test]
    fn invalid_enabled_value_is_reported_and_falls_through() {
        let cli = OtelCliOverrides {
            enabled: Some("maybe".to_string()),
            ..Default::default()
        };
        let file = OtelExportFile {
            enabled: Some(true),
            ..Default::default()
        };
        let r = resolve_with(&cli, Some(&file), &[]);
        assert!(r.config.enabled, "falls through to the file layer");
        assert!(matches!(r.issues[0], OtelConfigIssue::InvalidBool { .. }));
    }

    #[test]
    fn endpoint_precedence_cli_over_env_over_file() {
        let file = OtelExportFile {
            endpoint: Some("http://file:4318".to_string()),
            ..Default::default()
        };
        let env = [
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://env-base:4318"),
            (
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://env-traces:4318/custom",
            ),
        ];
        let cli = OtelCliOverrides {
            enabled: Some("true".to_string()),
            endpoint: Some("http://cli:4318".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_with(&cli, Some(&file), &env).config.endpoint,
            "http://cli:4318/v1/traces"
        );
        assert_eq!(
            resolve_with(&enabled_cli(), Some(&file), &env)
                .config
                .endpoint,
            "http://env-traces:4318/custom",
            "the signal-specific variable is used verbatim"
        );
        assert_eq!(
            resolve_with(&enabled_cli(), Some(&file), &env[..1])
                .config
                .endpoint,
            "http://env-base:4318/v1/traces"
        );
        assert_eq!(
            resolve_with(&enabled_cli(), Some(&file), &[])
                .config
                .endpoint,
            "http://file:4318/v1/traces"
        );
    }

    #[test]
    fn base_endpoint_keeps_its_path_and_tolerates_a_trailing_slash() {
        let r = resolve_with(
            &enabled_cli(),
            None,
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "https://api.example.test/otlp/",
            )],
        );
        assert_eq!(r.config.endpoint, "https://api.example.test/otlp/v1/traces");
    }

    #[test]
    fn grpc_endpoint_is_used_as_is_and_defaults_to_4317() {
        let env = [("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")];
        let r = resolve_with(&enabled_cli(), None, &env);
        assert_eq!(r.config.protocol, OtlpProtocol::Grpc);
        assert_eq!(r.config.endpoint, DEFAULT_GRPC_ENDPOINT);

        let r = resolve_with(
            &enabled_cli(),
            None,
            &[
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://jaeger:4317"),
            ],
        );
        assert_eq!(r.config.endpoint, "http://jaeger:4317/");
    }

    #[test]
    fn unusable_endpoint_disables_export_without_echoing_credentials() {
        let r = resolve_with(
            &enabled_cli(),
            None,
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "ftp://user:hunter2@collector/x?token=abc",
            )],
        );
        assert!(!r.config.enabled);
        let msg = r.issues.iter().map(ToString::to_string).collect::<String>();
        assert!(msg.contains("trace export is disabled"), "{msg}");
        assert!(!msg.contains("hunter2"), "{msg}");
        assert!(!msg.contains("abc"), "{msg}");

        let r = resolve_with(
            &enabled_cli(),
            None,
            &[("OTEL_EXPORTER_OTLP_ENDPOINT", "not a url")],
        );
        assert!(!r.config.enabled);
        assert!(r
            .issues
            .iter()
            .any(|i| matches!(i, OtelConfigIssue::InvalidEndpoint { .. })));
    }

    #[test]
    fn a_bad_endpoint_is_not_reported_when_export_is_off() {
        let r = resolve_with(
            &OtelCliOverrides::default(),
            None,
            &[("OTEL_EXPORTER_OTLP_ENDPOINT", "not a url")],
        );
        assert!(!r.config.enabled);
        assert!(r.issues.is_empty());
    }

    #[test]
    fn protocol_precedence_and_fallthrough_on_invalid_values() {
        let env = [
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
        ];
        assert_eq!(
            resolve_with(&enabled_cli(), None, &env).config.protocol,
            OtlpProtocol::HttpProtobuf,
            "signal-specific beats general"
        );
        let cli = OtelCliOverrides {
            protocol: Some("grpc".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_with(&cli, None, &env).config.protocol,
            OtlpProtocol::Grpc
        );

        let r = resolve_with(
            &enabled_cli(),
            None,
            &[
                ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/json"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ],
        );
        assert_eq!(r.config.protocol, OtlpProtocol::Grpc);
        assert!(matches!(
            r.issues[0],
            OtelConfigIssue::InvalidProtocol { .. }
        ));
    }

    #[test]
    fn service_name_precedence() {
        let file = OtelExportFile {
            service_name: Some("from-file".to_string()),
            ..Default::default()
        };
        let env = [
            ("OTEL_SERVICE_NAME", "from-env"),
            (
                "OTEL_RESOURCE_ATTRIBUTES",
                "service.name=from-attrs,team=core",
            ),
        ];
        let cli = OtelCliOverrides {
            service_name: Some("from-cli".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_with(&cli, Some(&file), &env).config.service_name,
            "from-cli"
        );
        assert_eq!(
            resolve_with(&OtelCliOverrides::default(), Some(&file), &env)
                .config
                .service_name,
            "from-env"
        );
        assert_eq!(
            resolve_with(&OtelCliOverrides::default(), Some(&file), &env[1..])
                .config
                .service_name,
            "from-attrs"
        );
        assert_eq!(
            resolve_with(&OtelCliOverrides::default(), Some(&file), &[])
                .config
                .service_name,
            "from-file"
        );
    }

    #[test]
    fn resource_attributes_merge_env_over_file_and_decode_percent_escapes() {
        let file = OtelExportFile {
            resource_attributes: BTreeMap::from([
                ("deployment.environment".to_string(), "staging".to_string()),
                ("team".to_string(), "file".to_string()),
            ]),
            ..Default::default()
        };
        let r = resolve_with(
            &OtelCliOverrides::default(),
            Some(&file),
            &[(
                "OTEL_RESOURCE_ATTRIBUTES",
                "deployment.environment=prod, note=a%20b%2Cc ,broken",
            )],
        );
        let attrs = &r.config.resource_attributes;
        assert_eq!(attrs["deployment.environment"], "prod");
        assert_eq!(attrs["team"], "file");
        assert_eq!(attrs["note"], "a b,c");
        assert!(!attrs.contains_key("service.name"));
        assert!(r
            .issues
            .iter()
            .any(|i| matches!(i, OtelConfigIssue::InvalidListEntry { index: 3, .. })));
    }

    #[test]
    fn sampler_precedence_cli_ratio_then_env_then_file_then_default() {
        let file = OtelExportFile {
            sample_ratio: Some(0.25),
            ..Default::default()
        };
        let env = [
            ("OTEL_TRACES_SAMPLER", "traceidratio"),
            ("OTEL_TRACES_SAMPLER_ARG", "0.5"),
        ];
        let cli = OtelCliOverrides {
            sample_ratio: Some("0.1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_with(&cli, Some(&file), &env).config.sampler,
            TracesSampler::ParentBasedTraceIdRatio(0.1)
        );
        assert_eq!(
            resolve_with(&OtelCliOverrides::default(), Some(&file), &env)
                .config
                .sampler,
            TracesSampler::TraceIdRatio(0.5)
        );
        assert_eq!(
            resolve_with(&OtelCliOverrides::default(), Some(&file), &[])
                .config
                .sampler,
            TracesSampler::ParentBasedTraceIdRatio(0.25)
        );
        assert_eq!(
            resolve_with(&OtelCliOverrides::default(), None, &[])
                .config
                .sampler,
            TracesSampler::ParentBasedAlwaysOn
        );
    }

    #[test]
    fn every_standard_sampler_name_parses() {
        let cases = [
            ("always_on", TracesSampler::AlwaysOn),
            ("always_off", TracesSampler::AlwaysOff),
            ("parentbased_always_on", TracesSampler::ParentBasedAlwaysOn),
            (
                "parentbased_always_off",
                TracesSampler::ParentBasedAlwaysOff,
            ),
            ("TraceIdRatio", TracesSampler::TraceIdRatio(1.0)),
            (
                "parentbased_traceidratio",
                TracesSampler::ParentBasedTraceIdRatio(1.0),
            ),
        ];
        for (name, expected) in cases {
            let r = resolve_with(
                &OtelCliOverrides::default(),
                None,
                &[("OTEL_TRACES_SAMPLER", name)],
            );
            assert_eq!(r.config.sampler, expected, "{name}");
            assert!(r.issues.is_empty(), "{name}: {:?}", r.issues);
        }
    }

    #[test]
    fn bad_sampler_inputs_are_reported_and_never_panic() {
        let r = resolve_with(
            &OtelCliOverrides::default(),
            None,
            &[("OTEL_TRACES_SAMPLER", "jaeger_remote")],
        );
        assert_eq!(r.config.sampler, TracesSampler::ParentBasedAlwaysOn);
        assert!(matches!(
            r.issues[0],
            OtelConfigIssue::InvalidSampler { .. }
        ));

        for bad in ["2", "-0.1", "nan", "inf", "abc"] {
            let r = resolve_with(
                &OtelCliOverrides::default(),
                None,
                &[
                    ("OTEL_TRACES_SAMPLER", "traceidratio"),
                    ("OTEL_TRACES_SAMPLER_ARG", bad),
                ],
            );
            assert_eq!(r.config.sampler, TracesSampler::TraceIdRatio(1.0), "{bad}");
            assert!(matches!(
                r.issues[0],
                OtelConfigIssue::InvalidSampleRatio { .. }
            ));

            let cli = OtelCliOverrides {
                sample_ratio: Some(bad.to_string()),
                ..Default::default()
            };
            let r = resolve_with(&cli, None, &[]);
            assert_eq!(
                r.config.sampler,
                TracesSampler::ParentBasedAlwaysOn,
                "{bad}"
            );
            assert!(matches!(
                r.issues[0],
                OtelConfigIssue::InvalidSampleRatio { .. }
            ));
        }
    }

    #[test]
    fn timeout_precedence_bounds_and_units() {
        let file = OtelExportFile {
            export_timeout_secs: Some(7),
            ..Default::default()
        };
        let r = resolve_with(
            &OtelCliOverrides::default(),
            Some(&file),
            &[
                ("OTEL_EXPORTER_OTLP_TIMEOUT", "2500"),
                ("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT", "1500"),
            ],
        );
        assert_eq!(r.config.export_timeout, Duration::from_millis(1500));
        let r = resolve_with(
            &OtelCliOverrides::default(),
            Some(&file),
            &[("OTEL_EXPORTER_OTLP_TIMEOUT", "2500")],
        );
        assert_eq!(r.config.export_timeout, Duration::from_millis(2500));
        assert_eq!(
            resolve_with(&OtelCliOverrides::default(), Some(&file), &[])
                .config
                .export_timeout,
            Duration::from_secs(7)
        );
        for bad in ["0", "999999999", "ten", "-5"] {
            let r = resolve_with(
                &OtelCliOverrides::default(),
                None,
                &[("OTEL_EXPORTER_OTLP_TIMEOUT", bad)],
            );
            assert_eq!(r.config.export_timeout, DEFAULT_EXPORT_TIMEOUT, "{bad}");
            assert!(matches!(
                r.issues[0],
                OtelConfigIssue::InvalidTimeout { .. }
            ));
        }
    }

    #[test]
    fn headers_are_parsed_decoded_lowercased_and_signal_specific_wins() {
        let r = resolve_with(
            &enabled_cli(),
            None,
            &[(
                "OTEL_EXPORTER_OTLP_HEADERS",
                "Authorization=Bearer%20tok-123, x-team=core",
            )],
        );
        let pairs: Vec<_> = r.config.headers.iter().collect();
        assert_eq!(
            pairs,
            vec![("authorization", "Bearer tok-123"), ("x-team", "core")]
        );

        let r = resolve_with(
            &enabled_cli(),
            None,
            &[
                ("OTEL_EXPORTER_OTLP_HEADERS", "a=general"),
                ("OTEL_EXPORTER_OTLP_TRACES_HEADERS", "a=specific"),
            ],
        );
        assert_eq!(
            r.config.headers.iter().collect::<Vec<_>>(),
            vec![("a", "specific")]
        );
    }

    #[test]
    fn duplicate_header_names_keep_the_last_value() {
        let r = resolve_with(
            &enabled_cli(),
            None,
            &[("OTEL_EXPORTER_OTLP_HEADERS", "x-k=one,X-K=two")],
        );
        assert_eq!(
            r.config.headers.iter().collect::<Vec<_>>(),
            vec![("x-k", "two")]
        );
    }

    #[test]
    fn malformed_and_unsafe_headers_are_dropped_without_leaking_values() {
        let secret = "s3cr3t-value";
        let raw = format!(
            "good=ok,novalue,=empty-key,bad name={secret},crlf=a%0D%0Ab-{secret},ctl=x%00{secret}"
        );
        let r = resolve_with(
            &enabled_cli(),
            None,
            &[("OTEL_EXPORTER_OTLP_HEADERS", &raw)],
        );
        assert_eq!(
            r.config.headers.iter().collect::<Vec<_>>(),
            vec![("good", "ok")]
        );
        assert!(r.issues.len() >= 5, "{:?}", r.issues);
        let rendered = format!(
            "{:?} {}",
            r.issues,
            r.issues.iter().map(ToString::to_string).collect::<String>()
        );
        assert!(
            !rendered.contains(secret),
            "issue leaked a header value: {rendered}"
        );
    }

    #[test]
    fn header_count_is_bounded() {
        let raw = (0..MAX_HEADERS + 10)
            .map(|i| format!("h{i}=v"))
            .collect::<Vec<_>>()
            .join(",");
        let r = resolve_with(
            &enabled_cli(),
            None,
            &[("OTEL_EXPORTER_OTLP_HEADERS", &raw)],
        );
        assert_eq!(r.config.headers.len(), MAX_HEADERS);
        assert!(r
            .issues
            .iter()
            .any(|i| matches!(i, OtelConfigIssue::TooManyEntries { .. })));
    }

    #[test]
    fn header_values_are_redacted_in_debug_display_and_summary() {
        let headers =
            OtlpHeaders::from_pairs(&[("authorization", "Bearer super-secret"), ("x-k", "v")]);
        assert_eq!(headers.to_string(), "[authorization=***, x-k=***]");
        assert!(!format!("{headers:?}").contains("super-secret"));

        let config = OtelExportConfig {
            enabled: true,
            headers,
            endpoint:
                "https://user:pw-secret@collector.example.test:4318/v1/traces?api_key=qs-secret"
                    .to_string(),
            ..Default::default()
        };
        for text in [
            format!("{config:?}"),
            format!("{config:#?}"),
            config.redacted_summary(),
        ] {
            assert!(!text.contains("super-secret"), "{text}");
            assert!(!text.contains("pw-secret"), "{text}");
            assert!(!text.contains("qs-secret"), "{text}");
            assert!(text.contains("authorization"), "names stay visible: {text}");
            assert!(text.contains("collector.example.test"), "{text}");
        }
    }

    #[test]
    fn redact_endpoint_strips_userinfo_query_and_fragment() {
        assert_eq!(
            redact_endpoint("https://u:p@host.example.test:4318/v1/traces?k=v#frag"),
            "https://***@host.example.test:4318/v1/traces?***"
        );
        assert_eq!(
            redact_endpoint("http://localhost:4317"),
            "http://localhost:4317/"
        );
        assert_eq!(redact_endpoint("::: nope :::"), "<unparsable endpoint>");
    }

    #[test]
    fn percent_decode_handles_edges() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%41%42"), "AB");
        assert_eq!(percent_decode(""), "");
    }

    #[test]
    fn empty_environment_values_count_as_unset() {
        let r = resolve_with(
            &enabled_cli(),
            None,
            &[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "   "),
                ("OTEL_SERVICE_NAME", ""),
                ("OTEL_EXPORTER_OTLP_HEADERS", ""),
            ],
        );
        assert!(r.issues.is_empty());
        assert_eq!(r.config.service_name, "temps");
        assert!(r.config.headers.is_empty());
    }

    #[test]
    fn config_file_parses_and_rejects_unknown_keys() {
        let file = parse_file(
            "enabled: true\nendpoint: http://collector:4318\nprotocol: http/protobuf\n\
             sample_ratio: 0.5\nexport_timeout_secs: 3\nresource_attributes:\n  env: prod\n",
        )
        .unwrap();
        assert_eq!(file.enabled, Some(true));
        assert_eq!(file.sample_ratio, Some(0.5));
        assert_eq!(file.resource_attributes["env"], "prod");

        assert_eq!(parse_file("  \n").unwrap(), OtelExportFile::default());
        assert!(parse_file("export_enabled: true").is_err());
        let err = parse_file("headers:\n  authorization: secret-token\n").unwrap_err();
        assert!(err.contains("OTEL_EXPORTER_OTLP_HEADERS"), "{err}");
        assert!(!err.contains("secret-token"), "{err}");
    }

    #[test]
    fn load_file_handles_missing_valid_and_broken_files() {
        let dir = std::env::temp_dir().join(format!("temps-otel-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(load_file(&dir), Ok(None));

        std::fs::write(config_file_path(&dir), "enabled: true\n").unwrap();
        let loaded = load_file(&dir).unwrap().unwrap();
        assert_eq!(loaded.enabled, Some(true));

        std::fs::write(config_file_path(&dir), "enabled: [unclosed").unwrap();
        let issue = load_file(&dir).unwrap_err();
        assert!(matches!(issue, OtelConfigIssue::InvalidConfigFile { .. }));
        assert!(issue.to_string().contains(CONFIG_FILE_NAME));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn protocol_serde_names_match_the_spec_values() {
        assert_eq!(
            serde_json::to_string(&OtlpProtocol::HttpProtobuf).unwrap(),
            "\"http/protobuf\""
        );
        assert_eq!(
            serde_json::to_string(&OtlpProtocol::Grpc).unwrap(),
            "\"grpc\""
        );
    }
}
