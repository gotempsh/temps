// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Export the Temps server's **own** spans over OTLP (`temps serve`).
//!
//! Settings are resolved by [`temps_config::otel_export`]; this module turns
//! the result into a `tracing` layer plus a tracer provider and owns the
//! provider's lifetime.
//!
//! # Cost model
//!
//! * **Disabled (the default):** nothing is built. No provider, no thread, no
//!   socket, no request middleware; the subscriber stack gains one
//!   `Option::None` layer slot.
//! * **Enabled:** spans are handed to the SDK's `BatchSpanProcessor`, which
//!   owns one OS thread and a **bounded** queue ([`MAX_QUEUE_SPANS`]). When
//!   the queue is full (collector slow or down) new spans are dropped, never
//!   buffered and never blocking the caller. Memory is therefore bounded by
//!   the queue plus one in-flight batch of [`MAX_EXPORT_BATCH_SPANS`]. gRPC
//!   additionally owns a single-worker tokio runtime for its channel.
//!
//! # What is exported
//!
//! Spans only. `tracing` *events* (log lines) are not exported: they carry
//! free-form text that can include identifiers and error strings, and this
//! feature ships data to a third-party backend. The HTTP request span records
//! method, **route template** and status code, never the URL, query string,
//! headers or body.
//!
//! # Failure policy
//!
//! Nothing in here may take the server down. Bad configuration is reported
//! as a warning and export stays off; an exporter that cannot reach its
//! collector logs a rate-limited warning and drops spans.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{
    BatchConfigBuilder, BatchSpanProcessor, Sampler, SdkTracerProvider, SpanData, SpanExporter,
};
use opentelemetry_sdk::Resource;
use temps_config::otel_export::{
    self as config, OtelCliOverrides, OtelExportConfig, OtlpProtocol, TracesSampler,
};
use thiserror::Error;
use tracing::Level;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::Registry;

/// Spans buffered before new ones are dropped.
pub const MAX_QUEUE_SPANS: usize = 2_048;
/// Spans per export request.
pub const MAX_EXPORT_BATCH_SPANS: usize = 512;
/// How often a partial batch is flushed.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Longest the process waits for the final flush at shutdown.
const MAX_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// While exports keep failing, warn at most this often.
const FAILURE_WARN_INTERVAL: Duration = Duration::from_secs(300);
/// Instrumentation scope reported with every span.
const TRACER_NAME: &str = "temps";
/// Longest error text copied into a log line.
const MAX_ERROR_CHARS: usize = 300;

/// The boxed layer installed into the subscriber. It sits directly on the
/// [`Registry`] so the same type works regardless of what is stacked above it.
pub type OtelLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// `temps serve` flags. All values are strings so that a typo is reported as
/// a warning by the resolver instead of aborting startup in clap.
///
/// OTLP headers deliberately have no flag: they are credentials and flag
/// values show up in `ps`. Set `OTEL_EXPORTER_OTLP_HEADERS` instead.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct OtelArgs {
    /// Export this server's own traces over OTLP. Off by default. Accepts
    /// `--otel-export` or `--otel-export=false`. The destination comes from
    /// --otel-endpoint or the standard OTEL_EXPORTER_OTLP_* variables.
    #[arg(
        long = "otel-export",
        env = "TEMPS_OTEL_EXPORT",
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    pub otel_export: Option<String>,

    /// OTLP collector base URL (overrides OTEL_EXPORTER_OTLP_ENDPOINT).
    /// For http/protobuf `/v1/traces` is appended.
    #[arg(long = "otel-endpoint", value_name = "URL")]
    pub otel_endpoint: Option<String>,

    /// OTLP transport: `grpc` or `http/protobuf` (overrides
    /// OTEL_EXPORTER_OTLP_PROTOCOL).
    #[arg(long = "otel-protocol", value_name = "PROTOCOL")]
    pub otel_protocol: Option<String>,

    /// `service.name` of exported spans (overrides OTEL_SERVICE_NAME).
    /// Default: temps.
    #[arg(long = "otel-service-name", value_name = "NAME")]
    pub otel_service_name: Option<String>,

    /// Fraction of traces to export, 0.0 to 1.0. A trace already sampled by
    /// the caller (W3C traceparent) is always kept. Overrides
    /// OTEL_TRACES_SAMPLER.
    #[arg(long = "otel-sample-ratio", value_name = "RATIO")]
    pub otel_sample_ratio: Option<String>,
}

impl OtelArgs {
    fn overrides(&self) -> OtelCliOverrides {
        OtelCliOverrides {
            enabled: self.otel_export.clone(),
            endpoint: self.otel_endpoint.clone(),
            protocol: self.otel_protocol.clone(),
            service_name: self.otel_service_name.clone(),
            sample_ratio: self.otel_sample_ratio.clone(),
        }
    }
}

/// Why the exporter could not be built. Every variant is non-fatal: the
/// caller logs it and the server runs without trace export.
#[derive(Debug, Error)]
pub enum OtelInitError {
    #[error("cannot start the {thread_name} runtime for the OTLP/gRPC exporter: {reason}")]
    Runtime { thread_name: String, reason: String },
    #[error("cannot build the HTTP client for the OTLP/HTTP exporter: {reason}")]
    HttpClient { reason: String },
    #[error("header '{name}' in the OTLP headers is not valid for gRPC metadata: {reason}")]
    InvalidGrpcHeader { name: String, reason: String },
    #[error("cannot build the {protocol} span exporter for {endpoint}: {reason}")]
    Exporter {
        protocol: OtlpProtocol,
        /// Already redacted.
        endpoint: String,
        reason: String,
    },
    #[error("building the OTLP exporter for {endpoint} panicked: {reason}")]
    Panicked { endpoint: String, reason: String },
}

/// Result of [`prepare`]: an optional layer plus messages to log once the
/// subscriber exists (nothing can be logged before it is installed).
pub struct OtelSetup {
    pub layer: Option<OtelLayer>,
    notes: Vec<(Level, String)>,
}

impl OtelSetup {
    fn disabled(notes: Vec<(Level, String)>) -> Self {
        Self { layer: None, notes }
    }

    /// Log the effective configuration (redacted) and every problem that was
    /// worked around. Call after the global subscriber is installed.
    pub fn report(&self) {
        for (level, message) in &self.notes {
            match *level {
                Level::ERROR => tracing::error!("{message}"),
                Level::WARN => tracing::warn!("{message}"),
                Level::INFO => tracing::info!("{message}"),
                _ => tracing::debug!("{message}"),
            }
        }
    }
}

/// Resolve configuration and, if export is enabled, build the provider and
/// layer. `args` is `None` for every command except `temps serve`.
pub fn prepare(args: Option<&OtelArgs>, data_dir: Option<&Path>) -> OtelSetup {
    let Some(args) = args else {
        return OtelSetup::disabled(Vec::new());
    };
    let data_dir = data_dir
        .map(Path::to_path_buf)
        .or_else(|| dirs::home_dir().map(|home| home.join(".temps")));

    let mut notes: Vec<(Level, String)> = Vec::new();
    let file = match data_dir.as_deref().map(config::load_file) {
        Some(Ok(file)) => file,
        Some(Err(issue)) => {
            notes.push((Level::WARN, issue.to_string()));
            None
        }
        None => None,
    };

    let resolution = config::resolve(&args.overrides(), file.as_ref(), &config::process_env);
    notes.extend(
        resolution
            .issues
            .iter()
            .map(|issue| (Level::WARN, issue.to_string())),
    );
    let cfg = resolution.config;
    if !cfg.enabled {
        return OtelSetup::disabled(notes);
    }

    match install(&cfg) {
        Ok(layer) => {
            notes.push((
                Level::INFO,
                format!(
                    "OpenTelemetry trace export enabled: {} (queue={MAX_QUEUE_SPANS} spans, \
                     batch={MAX_EXPORT_BATCH_SPANS}, flush every {FLUSH_INTERVAL:?})",
                    cfg.redacted_summary()
                ),
            ));
            OtelSetup {
                layer: Some(layer),
                notes,
            }
        }
        Err(error) => {
            notes.push((
                Level::WARN,
                format!("OpenTelemetry trace export is disabled: {error}"),
            ));
            OtelSetup::disabled(notes)
        }
    }
}

/// Everything that must outlive the subscriber.
struct Installed {
    provider: SdkTracerProvider,
    /// Present for gRPC only; the tonic channel lives on it.
    runtime: Option<tokio::runtime::Runtime>,
    shutdown_wait: Duration,
}

static INSTALLED: Mutex<Option<Installed>> = Mutex::new(None);
static REQUEST_SPANS: AtomicBool = AtomicBool::new(false);

fn install(cfg: &OtelExportConfig) -> Result<OtelLayer, OtelInitError> {
    let endpoint = config::redact_endpoint(&cfg.endpoint);
    // A panic inside an exporter constructor must not take the server down.
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_provider(cfg)))
        .map_err(|panic| OtelInitError::Panicked {
        endpoint,
        reason: panic_message(panic.as_ref()),
    })??;

    let (provider, runtime) = built;
    let layer = layer_for(&provider);

    let shutdown_wait = cfg.export_timeout.min(MAX_SHUTDOWN_WAIT);
    if let Ok(mut slot) = INSTALLED.lock() {
        *slot = Some(Installed {
            provider,
            runtime,
            shutdown_wait,
        });
    }
    REQUEST_SPANS.store(true, Ordering::Relaxed);
    Ok(layer)
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Targets whose spans must never be exported (the export path itself).
fn is_exporter_internal(target: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "opentelemetry",
        "tracing_opentelemetry",
        "tonic",
        "tower",
        "h2",
        "hyper",
        "reqwest",
        "rustls",
        "tokio_rustls",
    ];
    PREFIXES.iter().any(|prefix| {
        target == *prefix
            || target
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with("::") || rest.starts_with('_'))
    })
}

fn build_provider(
    cfg: &OtelExportConfig,
) -> Result<(SdkTracerProvider, Option<tokio::runtime::Runtime>), OtelInitError> {
    let redacted = config::redact_endpoint(&cfg.endpoint);
    let exporter_error = |reason: String| OtelInitError::Exporter {
        protocol: cfg.protocol,
        endpoint: redacted.clone(),
        reason,
    };

    let (exporter, runtime) = match cfg.protocol {
        OtlpProtocol::HttpProtobuf => {
            // The blocking client must be created outside any async context;
            // `prepare` runs before the serve runtime exists.
            let client = reqwest_otlp::blocking::Client::builder()
                .timeout(cfg.export_timeout)
                .build()
                .map_err(|e| OtelInitError::HttpClient {
                    reason: e.to_string(),
                })?;
            let headers: HashMap<String, String> = cfg
                .headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_http_client(client)
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                .with_endpoint(cfg.endpoint.clone())
                .with_timeout(cfg.export_timeout)
                .with_headers(headers)
                .build()
                .map_err(|e| exporter_error(e.to_string()))?;
            (exporter, None)
        }
        OtlpProtocol::Grpc => {
            use axum::http::{HeaderMap, HeaderName, HeaderValue};
            use opentelemetry_otlp::tonic_types::metadata::MetadataMap;
            let mut header_map = HeaderMap::new();
            for (name, value) in cfg.headers.iter() {
                // Errors name the header but never echo its value.
                let key = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                    OtelInitError::InvalidGrpcHeader {
                        name: name.to_string(),
                        reason: e.to_string(),
                    }
                })?;
                let value =
                    HeaderValue::from_str(value).map_err(|_| OtelInitError::InvalidGrpcHeader {
                        name: name.to_string(),
                        reason: "the value contains characters gRPC metadata cannot carry"
                            .to_string(),
                    })?;
                header_map.insert(key, value);
            }
            let metadata = MetadataMap::from_headers(header_map);

            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("temps-otlp")
                .enable_all()
                .build()
                .map_err(|e| OtelInitError::Runtime {
                    thread_name: "temps-otlp".to_string(),
                    reason: e.to_string(),
                })?;
            // The tonic channel is created lazily: an unreachable collector
            // does not fail the build, it fails individual exports.
            let built = runtime.block_on(async {
                let mut builder = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(cfg.endpoint.clone())
                    .with_timeout(cfg.export_timeout)
                    .with_metadata(metadata);
                if cfg.endpoint.starts_with("https://") {
                    builder = builder.with_tls_config(
                        opentelemetry_otlp::tonic_types::transport::ClientTlsConfig::new()
                            .with_enabled_roots(),
                    );
                }
                builder.build()
            });
            match built {
                Ok(exporter) => (exporter, Some(runtime)),
                Err(e) => {
                    runtime.shutdown_background();
                    return Err(exporter_error(e.to_string()));
                }
            }
        }
    };

    Ok((provider_with_exporter(exporter, cfg), runtime))
}

/// Provider with the bounded batch processor, resource and sampler from
/// `cfg`. Generic over the exporter so tests can use an in-memory one.
fn provider_with_exporter<E: SpanExporter + 'static>(
    exporter: E,
    cfg: &OtelExportConfig,
) -> SdkTracerProvider {
    let exporter = ReportingExporter::new(exporter, &cfg.endpoint);
    let processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_queue_size(MAX_QUEUE_SPANS)
                .with_max_export_batch_size(MAX_EXPORT_BATCH_SPANS)
                .with_scheduled_delay(FLUSH_INTERVAL)
                .build(),
        )
        .build();
    SdkTracerProvider::builder()
        .with_resource(build_resource(cfg))
        .with_sampler(to_sdk_sampler(cfg.sampler))
        .with_span_processor(processor)
        .build()
}

/// The `tracing` layer feeding `provider`.
fn layer_for(provider: &SdkTracerProvider) -> OtelLayer {
    tracing_opentelemetry::layer()
        .with_tracer(provider.tracer(TRACER_NAME))
        .with_location(false)
        .with_threads(false)
        // Spans only (see module docs), and never the exporter's own
        // transport crates: exporting their spans would feed the exporter.
        .with_filter(filter_fn(|meta| {
            meta.is_span() && !is_exporter_internal(meta.target())
        }))
        .boxed()
}

/// `service.name`, `service.version` and a per-process `service.instance.id`,
/// then host and operator-supplied attributes (which may override any of
/// them except `service.name`, which already has its own precedence).
///
/// Built from scratch rather than `Resource::builder()` so the SDK's own
/// `OTEL_*` detectors cannot bypass the precedence resolved in `temps-config`.
fn build_resource(cfg: &OtelExportConfig) -> Resource {
    let mut attributes: BTreeMap<String, String> = BTreeMap::new();
    attributes.insert(
        "service.version".to_string(),
        env!("TEMPS_VERSION").to_string(),
    );
    attributes.insert(
        "service.instance.id".to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    attributes.insert("process.pid".to_string(), std::process::id().to_string());
    if let Ok(host) = gethostname::gethostname().into_string() {
        attributes.insert("host.name".to_string(), host);
    }
    attributes.extend(cfg.resource_attributes.clone());
    attributes.insert("service.name".to_string(), cfg.service_name.clone());

    Resource::builder_empty()
        .with_attributes(
            attributes
                .into_iter()
                .map(|(k, v)| KeyValue::new(k, v))
                .collect::<Vec<_>>(),
        )
        .build()
}

fn to_sdk_sampler(sampler: TracesSampler) -> Sampler {
    match sampler {
        TracesSampler::AlwaysOn => Sampler::AlwaysOn,
        TracesSampler::AlwaysOff => Sampler::AlwaysOff,
        TracesSampler::TraceIdRatio(ratio) => Sampler::TraceIdRatioBased(ratio),
        TracesSampler::ParentBasedAlwaysOn => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
        TracesSampler::ParentBasedAlwaysOff => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        TracesSampler::ParentBasedTraceIdRatio(ratio) => {
            Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
        }
    }
}

/// Flush queued spans and stop the exporter. Idempotent; a no-op when export
/// is disabled. Blocks for at most a few seconds. Call it from a thread where
/// blocking is allowed (the async [`shutdown`] does that).
pub fn shutdown_blocking() {
    let installed = match INSTALLED.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    let Some(installed) = installed else { return };
    REQUEST_SPANS.store(false, Ordering::Relaxed);
    match installed
        .provider
        .shutdown_with_timeout(installed.shutdown_wait)
    {
        Ok(()) => tracing::debug!("OpenTelemetry tracer provider flushed and shut down"),
        Err(e) => tracing::warn!(
            "OpenTelemetry tracer provider did not shut down cleanly; the last spans may be lost: {e}"
        ),
    }
    if let Some(runtime) = installed.runtime {
        // Never blocks, and is safe to call from inside an async context.
        runtime.shutdown_background();
    }
}

/// Async-friendly [`shutdown_blocking`]: runs on the blocking pool with an
/// overall deadline so a stuck collector cannot hold up process exit.
pub async fn shutdown() {
    let deadline = MAX_SHUTDOWN_WAIT + Duration::from_secs(1);
    let flush = tokio::task::spawn_blocking(shutdown_blocking);
    if tokio::time::timeout(deadline, flush).await.is_err() {
        tracing::warn!(
            "OpenTelemetry flush exceeded {deadline:?}; continuing shutdown without waiting"
        );
    }
}

/// Whether HTTP request spans should be installed on the console routers.
pub fn request_tracing_enabled() -> bool {
    REQUEST_SPANS.load(Ordering::Relaxed)
}

/// Add the request-span middleware when export is enabled; otherwise return
/// the router untouched.
pub fn layer_requests(router: axum::Router) -> axum::Router {
    if request_tracing_enabled() {
        router.layer(axum::middleware::from_fn(trace_http_request))
    } else {
        router
    }
}

/// Server span per console request, parented on an incoming W3C
/// `traceparent` when there is one.
///
/// Attributes are limited to values with bounded cardinality that cannot
/// carry secrets: HTTP method, the route *template* (`/api/projects/{id}`,
/// never the concrete path or query) and the response status.
///
/// Caveat: a client on the internet can send a `traceparent` whose sampled
/// flag is set, and a parent-based sampler (the default, and what
/// `--otel-sample-ratio` uses) then exports that request regardless of the
/// ratio. The bounded queue caps the cost. To ignore callers' sampling
/// decisions, use `OTEL_TRACES_SAMPLER=traceidratio` (not parent-based).
pub async fn trace_http_request(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tracing::Instrument;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let method = known_method(req.method());
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_string());

    let span = tracing::info_span!(
        target: "temps_cli::http_server",
        "http.request",
        otel.name = %format_args!("{method} {route}"),
        otel.kind = "server",
        http.request.method = method,
        http.route = %route,
        http.response.status_code = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    );
    if req.headers().contains_key("traceparent") {
        let parent = TraceContextPropagator::new().extract(&HeaderExtractor(req.headers()));
        // Failing to set the parent only loses the link to the caller's trace.
        let _ = span.set_parent(parent);
    }

    let response = next.run(req).instrument(span.clone()).await;
    let status = response.status();
    span.record("http.response.status_code", status.as_u16());
    if status.is_server_error() {
        span.record("otel.status_code", "ERROR");
    }
    response
}

use opentelemetry::propagation::{Extractor, TextMapPropagator};
use opentelemetry_sdk::propagation::TraceContextPropagator;

/// Reads W3C trace-context headers out of an `http::HeaderMap`.
struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// HTTP method as a bounded label: the standard methods, `_OTHER` otherwise
/// (OpenTelemetry semantic conventions), so a client cannot mint new values.
fn known_method(method: &axum::http::Method) -> &'static str {
    match method.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "CONNECT" => "CONNECT",
        "TRACE" => "TRACE",
        _ => "_OTHER",
    }
}

/// Wraps the real exporter to turn silent failures into a rate-limited
/// warning, since the SDK's own diagnostics are not wired into `tracing`.
#[derive(Debug)]
struct ReportingExporter<E> {
    inner: E,
    /// Raw URL, kept only to scrub it out of error text.
    raw_endpoint: String,
    redacted_endpoint: String,
    failures: Mutex<FailureState>,
}

#[derive(Debug, Default)]
struct FailureState {
    failing: bool,
    last_warned: Option<Instant>,
    /// Failed exports since the last warning.
    suppressed: u64,
}

impl<E> ReportingExporter<E> {
    fn new(inner: E, endpoint: &str) -> Self {
        Self {
            inner,
            raw_endpoint: endpoint.to_string(),
            redacted_endpoint: config::redact_endpoint(endpoint),
            failures: Mutex::new(FailureState::default()),
        }
    }

    fn scrub(&self, text: &str) -> String {
        let mut text = text.replace(&self.raw_endpoint, &self.redacted_endpoint);
        if text.chars().count() > MAX_ERROR_CHARS {
            text = text.chars().take(MAX_ERROR_CHARS).collect::<String>() + "...";
        }
        text
    }

    fn record(&self, spans: usize, result: &OTelSdkResult) {
        let Ok(mut state) = self.failures.lock() else {
            return;
        };
        match result {
            Ok(()) => {
                if state.failing {
                    state.failing = false;
                    state.last_warned = None;
                    state.suppressed = 0;
                    tracing::info!(
                        target: "temps_cli::otel_export",
                        endpoint = %self.redacted_endpoint,
                        "OTLP trace export recovered"
                    );
                }
            }
            Err(error) => {
                state.failing = true;
                let due = state
                    .last_warned
                    .is_none_or(|at| at.elapsed() >= FAILURE_WARN_INTERVAL);
                if due {
                    let suppressed = std::mem::take(&mut state.suppressed);
                    state.last_warned = Some(Instant::now());
                    tracing::warn!(
                        target: "temps_cli::otel_export",
                        endpoint = %self.redacted_endpoint,
                        dropped_spans = spans,
                        further_failed_exports = suppressed,
                        error = %self.scrub(&error.to_string()),
                        "OTLP trace export failed; spans in this batch are dropped. The server is \
                         unaffected. Check the endpoint, protocol and credentials \
                         (further failures are logged at most every {}s)",
                        FAILURE_WARN_INTERVAL.as_secs()
                    );
                } else {
                    state.suppressed += 1;
                }
            }
        }
    }
}

impl<E: SpanExporter> SpanExporter for ReportingExporter<E> {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let spans = batch.len();
        let result = self.inner.export(batch).await;
        self.record(spans, &result);
        result
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use clap::Parser;
    use opentelemetry::trace::{SpanId, SpanKind, Status};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, InMemorySpanExporterBuilder};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use temps_config::otel_export::{resolve, OtelCliOverrides};
    use tower::ServiceExt;
    use tracing_subscriber::layer::SubscriberExt;

    const TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    fn enabled_config() -> OtelExportConfig {
        OtelExportConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn capture(
        cfg: &OtelExportConfig,
    ) -> (
        SdkTracerProvider,
        InMemorySpanExporter,
        impl tracing::Subscriber + Send + Sync,
    ) {
        let memory = InMemorySpanExporterBuilder::new().build();
        let provider = provider_with_exporter(memory.clone(), cfg);
        let subscriber = tracing_subscriber::registry().with(layer_for(&provider));
        (provider, memory, subscriber)
    }

    fn finished(provider: &SdkTracerProvider, memory: &InMemorySpanExporter) -> Vec<SpanData> {
        provider.force_flush().expect("flush in-memory exporter");
        memory.get_finished_spans().expect("read finished spans")
    }

    fn test_app() -> Router {
        Router::new()
            .route("/api/projects/{id}", get(|| async { "ok" }))
            .route("/boom", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
            .layer(axum::middleware::from_fn(trace_http_request))
    }

    fn attribute(span: &SpanData, key: &str) -> Option<String> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }

    // ---- CLI ---------------------------------------------------------

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        otel: OtelArgs,
    }

    #[test]
    fn flags_parse_in_every_form_and_default_to_unset() {
        let none = TestCli::try_parse_from(["t"]).unwrap().otel;
        if std::env::var_os("TEMPS_OTEL_EXPORT").is_none() {
            assert_eq!(none.otel_export, None);
        }
        assert_eq!(none.otel_endpoint, None);

        let bare = TestCli::try_parse_from(["t", "--otel-export"])
            .unwrap()
            .otel;
        assert_eq!(bare.otel_export.as_deref(), Some("true"));

        let off = TestCli::try_parse_from(["t", "--otel-export=false"])
            .unwrap()
            .otel;
        assert_eq!(off.otel_export.as_deref(), Some("false"));

        let all = TestCli::try_parse_from([
            "t",
            "--otel-export",
            "--otel-endpoint",
            "http://collector:4318",
            "--otel-protocol",
            "grpc",
            "--otel-service-name",
            "svc",
            "--otel-sample-ratio",
            "0.5",
        ])
        .unwrap()
        .otel;
        let o = all.overrides();
        assert_eq!(o.enabled.as_deref(), Some("true"));
        assert_eq!(o.endpoint.as_deref(), Some("http://collector:4318"));
        assert_eq!(o.protocol.as_deref(), Some("grpc"));
        assert_eq!(o.service_name.as_deref(), Some("svc"));
        assert_eq!(o.sample_ratio.as_deref(), Some("0.5"));
    }

    #[test]
    fn a_malformed_flag_value_does_not_abort_parsing() {
        // Typos must surface as warnings from the resolver, not as a clap
        // error that stops `temps serve` from starting.
        let args = TestCli::try_parse_from([
            "t",
            "--otel-sample-ratio",
            "lots",
            "--otel-protocol",
            "carrier-pigeon",
            "--otel-export=perhaps",
        ])
        .expect("string-typed flags always parse")
        .otel;
        let resolution = resolve(&args.overrides(), None, &|_| None);
        assert!(!resolution.config.enabled);
        assert!(!resolution.issues.is_empty());
    }

    // ---- layer + provider -------------------------------------------

    #[test]
    fn spans_are_exported_but_log_events_are_not() {
        let (provider, memory, subscriber) = capture(&enabled_config());
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("unit_of_work", answer = 42);
            let _entered = span.enter();
            tracing::info!("log line that must stay local");
        });
        let spans = finished(&provider, &memory);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "unit_of_work");
        assert_eq!(attribute(&spans[0], "answer").as_deref(), Some("42"));
        assert!(
            spans[0].events.is_empty(),
            "events must not be exported: {:?}",
            spans[0].events
        );
    }

    #[test]
    fn spans_from_the_exporter_transport_crates_are_not_exported() {
        let (provider, memory, subscriber) = capture(&enabled_config());
        tracing::subscriber::with_default(subscriber, || {
            drop(tracing::info_span!(target: "hyper::client::conn", "dial"));
            drop(tracing::info_span!(target: "tonic::transport", "channel"));
            drop(tracing::info_span!(target: "h2::codec", "frame"));
            drop(tracing::info_span!(target: "temps_deployments::jobs", "kept"));
        });
        let names: Vec<_> = finished(&provider, &memory)
            .into_iter()
            .map(|s| s.name.to_string())
            .collect();
        assert_eq!(names, vec!["kept"]);
    }

    #[test]
    fn exporter_internal_target_matching_is_exact_on_module_boundaries() {
        for internal in [
            "hyper",
            "hyper::proto::h1",
            "hyper_util::client",
            "h2::codec::framed_write",
            "tonic::transport::channel",
            "reqwest::connect",
            "opentelemetry_sdk::trace",
            "tracing_opentelemetry::layer",
            "tower::buffer",
        ] {
            assert!(is_exporter_internal(internal), "{internal}");
        }
        for ours in [
            "temps_cli::http_server",
            "temps_deployments",
            "hyperion",
            "h22",
            "toniclike",
            "my_reqwest",
        ] {
            assert!(!is_exporter_internal(ours), "{ours}");
        }
    }

    #[test]
    fn resource_carries_service_identity_and_operator_attributes() {
        let mut cfg = enabled_config();
        cfg.service_name = "temps-test".to_string();
        cfg.resource_attributes
            .insert("deployment.environment".to_string(), "staging".to_string());
        let resource = build_resource(&cfg);
        let get = |key: &'static str| {
            resource
                .get(&opentelemetry::Key::from_static_str(key))
                .map(|v| v.to_string())
        };
        assert_eq!(get("service.name").as_deref(), Some("temps-test"));
        assert_eq!(
            get("service.version").as_deref(),
            Some(env!("TEMPS_VERSION"))
        );
        assert_eq!(get("deployment.environment").as_deref(), Some("staging"));
        assert!(get("service.instance.id").is_some_and(|id| id.len() >= 32));
        assert!(get("process.pid").is_some());

        // The id is per build of the resource, i.e. per process start.
        let other = build_resource(&cfg)
            .get(&opentelemetry::Key::from_static_str("service.instance.id"))
            .map(|v| v.to_string());
        assert_ne!(get("service.instance.id"), other);

        // Operators may override defaults, but not service.name.
        cfg.resource_attributes
            .insert("service.version".to_string(), "pinned".to_string());
        cfg.resource_attributes
            .insert("service.name".to_string(), "ignored".to_string());
        let resource = build_resource(&cfg);
        assert_eq!(
            resource
                .get(&opentelemetry::Key::from_static_str("service.version"))
                .map(|v| v.to_string())
                .as_deref(),
            Some("pinned")
        );
        assert_eq!(
            resource
                .get(&opentelemetry::Key::from_static_str("service.name"))
                .map(|v| v.to_string())
                .as_deref(),
            Some("temps-test")
        );
    }

    #[test]
    fn samplers_map_and_always_off_exports_nothing() {
        let mut cfg = enabled_config();
        cfg.sampler = TracesSampler::AlwaysOff;
        let (provider, memory, subscriber) = capture(&cfg);
        tracing::subscriber::with_default(subscriber, || {
            drop(tracing::info_span!("dropped"));
        });
        assert!(finished(&provider, &memory).is_empty());

        assert!(matches!(
            to_sdk_sampler(TracesSampler::ParentBasedTraceIdRatio(0.5)),
            Sampler::ParentBased(_)
        ));
        assert!(matches!(
            to_sdk_sampler(TracesSampler::TraceIdRatio(0.5)),
            Sampler::TraceIdRatioBased(r) if r == 0.5
        ));
    }

    // ---- HTTP request span ------------------------------------------

    #[tokio::test(flavor = "current_thread")]
    async fn request_span_uses_the_route_template_and_continues_the_callers_trace() {
        let (provider, memory, subscriber) = capture(&enabled_config());
        let _guard = tracing::subscriber::set_default(subscriber);

        let request = Request::builder()
            .uri("/api/projects/12345?token=hunter2")
            .header("traceparent", TRACEPARENT)
            .header("authorization", "Bearer s3cr3t-credential")
            .header("cookie", "session=s3cr3t-cookie")
            .body(Body::empty())
            .unwrap();
        let response = test_app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let spans = finished(&provider, &memory);
        assert_eq!(spans.len(), 1, "{spans:?}");
        let span = &spans[0];
        assert_eq!(span.name, "GET /api/projects/{id}");
        assert_eq!(span.span_kind, SpanKind::Server);
        assert_eq!(
            span.span_context.trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(span.parent_span_id.to_string(), "00f067aa0ba902b7");
        assert_eq!(
            attribute(span, "http.route").as_deref(),
            Some("/api/projects/{id}")
        );
        assert_eq!(
            attribute(span, "http.request.method").as_deref(),
            Some("GET")
        );
        assert_eq!(
            attribute(span, "http.response.status_code").as_deref(),
            Some("200")
        );

        // Nothing request-specific may leak into the span.
        let everything = format!("{span:?}");
        for needle in ["12345", "hunter2", "s3cr3t", "token="] {
            assert!(
                !everything.contains(needle),
                "{needle} leaked: {everything}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_without_traceparent_starts_a_new_trace_and_5xx_is_an_error() {
        let (provider, memory, subscriber) = capture(&enabled_config());
        let _guard = tracing::subscriber::set_default(subscriber);

        let response = test_app()
            .oneshot(Request::builder().uri("/boom").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // Unknown paths collapse to one bounded name, whatever the client sends.
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("BREW")
                    .uri("/no/such/route/ab12cd34")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let spans = finished(&provider, &memory);
        assert_eq!(spans.len(), 2, "{spans:?}");
        let boom = spans.iter().find(|s| s.name == "GET /boom").unwrap();
        assert_eq!(boom.parent_span_id, SpanId::INVALID);
        assert!(
            matches!(boom.status, Status::Error { .. }),
            "{:?}",
            boom.status
        );
        let unmatched = spans
            .iter()
            .find(|s| s.name == "_OTHER unmatched")
            .expect("unknown method and path are bounded");
        assert!(!matches!(unmatched.status, Status::Error { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ratio_sampling_keeps_sampled_callers_and_drops_unsampled_roots() {
        let mut cfg = enabled_config();
        cfg.sampler = TracesSampler::ParentBasedTraceIdRatio(0.0);
        let (provider, memory, subscriber) = capture(&cfg);
        let _guard = tracing::subscriber::set_default(subscriber);

        let root = Request::builder()
            .uri("/api/projects/1")
            .body(Body::empty())
            .unwrap();
        test_app().oneshot(root).await.unwrap();
        assert!(finished(&provider, &memory).is_empty());

        let sampled = Request::builder()
            .uri("/api/projects/1")
            .header("traceparent", TRACEPARENT)
            .body(Body::empty())
            .unwrap();
        test_app().oneshot(sampled).await.unwrap();
        assert_eq!(finished(&provider, &memory).len(), 1);
    }

    #[test]
    fn unknown_http_methods_collapse_to_one_label() {
        assert_eq!(known_method(&axum::http::Method::GET), "GET");
        assert_eq!(known_method(&axum::http::Method::PATCH), "PATCH");
        let custom = axum::http::Method::from_bytes(b"X-RANDOM-1234").unwrap();
        assert_eq!(known_method(&custom), "_OTHER");
    }

    // ---- failure policy ---------------------------------------------

    #[test]
    fn failure_logging_is_rate_limited_and_reports_recovery() {
        let exporter = ReportingExporter::new(
            InMemorySpanExporterBuilder::new().build(),
            "https://user:pw@collector.example.test/v1/traces?key=abc",
        );
        let failure: OTelSdkResult = Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
            "connection refused".to_string(),
        ));

        exporter.record(10, &failure);
        {
            let state = exporter.failures.lock().unwrap();
            assert!(state.failing);
            assert_eq!(state.suppressed, 0, "the first failure is logged");
            assert!(state.last_warned.is_some());
        }
        for _ in 0..4 {
            exporter.record(10, &failure);
        }
        assert_eq!(exporter.failures.lock().unwrap().suppressed, 4);

        exporter.record(10, &Ok(()));
        let state = exporter.failures.lock().unwrap();
        assert!(!state.failing);
        assert_eq!(state.suppressed, 0);
        assert!(state.last_warned.is_none());
    }

    #[test]
    fn error_text_is_scrubbed_of_the_raw_endpoint_and_truncated() {
        let raw = "https://user:pw-secret@collector.example.test/v1/traces?key=abc";
        let exporter = ReportingExporter::new(InMemorySpanExporterBuilder::new().build(), raw);
        let scrubbed = exporter.scrub(&format!("error sending request for url ({raw})"));
        assert!(!scrubbed.contains("pw-secret"), "{scrubbed}");
        assert!(!scrubbed.contains("key=abc"), "{scrubbed}");
        assert!(scrubbed.contains("collector.example.test"), "{scrubbed}");

        let long = exporter.scrub(&"x".repeat(5_000));
        assert!(long.chars().count() <= MAX_ERROR_CHARS + 3);
    }

    #[test]
    fn unreachable_collector_over_http_neither_panics_nor_hangs() {
        let mut cfg = enabled_config();
        cfg.endpoint = "http://127.0.0.1:1/v1/traces".to_string();
        cfg.export_timeout = Duration::from_millis(500);
        let (provider, runtime) =
            build_provider(&cfg).expect("an unreachable collector still builds");
        assert!(runtime.is_none());

        let subscriber = tracing_subscriber::registry().with(layer_for(&provider));
        tracing::subscriber::with_default(subscriber, || {
            drop(tracing::info_span!("goes_nowhere"));
        });
        let started = Instant::now();
        let _ = provider.shutdown_with_timeout(Duration::from_secs(3));
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "shutdown took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn grpc_exporter_builds_lazily_against_an_unreachable_collector() {
        let mut cfg = enabled_config();
        cfg.protocol = OtlpProtocol::Grpc;
        cfg.endpoint = "http://127.0.0.1:1".to_string();
        cfg.export_timeout = Duration::from_millis(500);
        let (provider, runtime) = build_provider(&cfg).expect("gRPC channel connects lazily");
        let runtime = runtime.expect("gRPC owns a runtime");

        let subscriber = tracing_subscriber::registry().with(layer_for(&provider));
        tracing::subscriber::with_default(subscriber, || {
            drop(tracing::info_span!("goes_nowhere"));
        });
        let started = Instant::now();
        let _ = provider.shutdown_with_timeout(Duration::from_secs(3));
        runtime.shutdown_background();
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn grpc_headers_that_metadata_cannot_carry_are_rejected_without_the_value() {
        let secret = "line one\nsecret-after-newline";
        let env = move |key: &str| match key {
            "OTEL_EXPORTER_OTLP_HEADERS" => Some(format!("x-key={secret}")),
            _ => None,
        };
        // The resolver already drops control characters, so nothing reaches
        // the exporter; the assertion is that the secret appears nowhere.
        let cli = OtelCliOverrides {
            enabled: Some("true".to_string()),
            protocol: Some("grpc".to_string()),
            ..Default::default()
        };
        let resolution = resolve(&cli, None, &env);
        let rendered = format!(
            "{:?} {:?}",
            resolution.config,
            resolution
                .issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        assert!(!rendered.contains("secret-after-newline"), "{rendered}");
    }

    // ---- end to end against a local fake collector -------------------

    struct CapturedRequest {
        request_line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    /// Minimal one-shot OTLP/HTTP collector: records the first request and
    /// answers 200.
    fn fake_collector() -> (u16, mpsc::Receiver<CapturedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or_default().to_string();
            let headers: Vec<(String, String)> = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let length: usize = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            let mut body = buf[header_end + 4..].to_vec();
            while body.len() < length {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => body.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
            let _ = tx.send(CapturedRequest {
                request_line,
                headers,
                body,
            });
        });
        (port, rx)
    }

    #[test]
    fn spans_reach_an_otlp_http_collector_with_resource_and_auth_header() {
        let (port, received) = fake_collector();
        let env = move |key: &str| match key {
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(format!("http://127.0.0.1:{port}")),
            "OTEL_EXPORTER_OTLP_HEADERS" => Some("Authorization=Bearer%20test-token".to_string()),
            "OTEL_SERVICE_NAME" => Some("temps-e2e".to_string()),
            _ => None,
        };
        let cli = OtelCliOverrides {
            enabled: Some("true".to_string()),
            ..Default::default()
        };
        let resolution = resolve(&cli, None, &env);
        assert!(resolution.issues.is_empty(), "{:?}", resolution.issues);
        let cfg = resolution.config;
        assert!(cfg.enabled);

        let (provider, runtime) = build_provider(&cfg).unwrap();
        assert!(runtime.is_none());
        let subscriber = tracing_subscriber::registry().with(layer_for(&provider));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("e2e_unit_of_work");
            let _entered = span.enter();
        });
        provider
            .shutdown_with_timeout(Duration::from_secs(5))
            .unwrap();

        let request = received
            .recv_timeout(Duration::from_secs(10))
            .expect("the collector received an export request");
        assert_eq!(request.request_line, "POST /v1/traces HTTP/1.1");
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(header("authorization"), Some("Bearer test-token"));
        assert_eq!(header("content-type"), Some("application/x-protobuf"));
        let contains = |needle: &str| {
            request
                .body
                .windows(needle.len())
                .any(|w| w == needle.as_bytes())
        };
        assert!(
            contains("e2e_unit_of_work"),
            "span name missing from payload"
        );
        assert!(contains("temps-e2e"), "service.name missing from payload");
        assert!(contains("service.instance.id"));
        assert!(contains(env!("TEMPS_VERSION")), "service.version missing");
        assert!(
            !contains("test-token"),
            "credentials must not be in the payload"
        );
    }

    // ---- prepare / lifecycle -----------------------------------------

    #[test]
    fn prepare_is_free_for_commands_that_do_not_export() {
        let setup = prepare(None, None);
        assert!(setup.layer.is_none());
        assert!(setup.notes.is_empty());
    }

    #[test]
    fn prepare_enables_from_the_config_file_and_shuts_down_cleanly() {
        // The only test that touches the process-wide install state.
        assert!(!request_tracing_enabled());
        shutdown_blocking(); // nothing installed: a harmless no-op

        let dir = std::env::temp_dir().join(format!("temps-otel-prepare-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Disabled by default: no layer, no middleware.
        let off = prepare(Some(&OtelArgs::default()), Some(&dir));
        assert!(off.layer.is_none());
        assert!(!request_tracing_enabled());

        // A broken file is a warning, not a failure.
        std::fs::write(config::config_file_path(&dir), "enabled: [oops").unwrap();
        let broken = prepare(Some(&OtelArgs::default()), Some(&dir));
        assert!(broken.layer.is_none());
        assert!(broken
            .notes
            .iter()
            .any(|(level, text)| *level == Level::WARN && text.contains("otel-export.yaml")));

        // Enabled through the file, pointing at a closed port.
        std::fs::write(
            config::config_file_path(&dir),
            "enabled: true\nendpoint: http://127.0.0.1:1\nexport_timeout_secs: 1\n",
        )
        .unwrap();
        let on = prepare(Some(&OtelArgs::default()), Some(&dir));
        if std::env::var_os("OTEL_SDK_DISABLED").is_none() {
            assert!(on.layer.is_some(), "{:?}", on.notes);
            assert!(request_tracing_enabled());
            assert!(on
                .notes
                .iter()
                .any(|(level, text)| *level == Level::INFO && text.contains("enabled")));
            // The CLI flag outranks the file.
            shutdown_blocking();
            assert!(!request_tracing_enabled());
            let flagged = OtelArgs {
                otel_export: Some("false".to_string()),
                ..Default::default()
            };
            assert!(prepare(Some(&flagged), Some(&dir)).layer.is_none());
        }
        shutdown_blocking();
        shutdown_blocking(); // idempotent
        assert!(!request_tracing_enabled());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unusable_endpoint_leaves_export_off_and_says_why() {
        let dir = std::env::temp_dir().join(format!("temps-otel-badurl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            config::config_file_path(&dir),
            "enabled: true\nendpoint: 'ftp://user:topsecret@collector/x'\n",
        )
        .unwrap();
        let setup = prepare(Some(&OtelArgs::default()), Some(&dir));
        if std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_none()
            && std::env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_none()
        {
            assert!(setup.layer.is_none());
            let text: String = setup.notes.iter().map(|(_, t)| t.as_str()).collect();
            assert!(text.contains("disabled"), "{text}");
            assert!(!text.contains("topsecret"), "{text}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
