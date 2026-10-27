// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Select, Set, TransactionTrait,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use temps_config::ConfigService;
use temps_core::environment_address::{
    https_url, AddressKind, EnvironmentAddress, EnvironmentAddressSlot, EnvironmentAddresses,
};
use temps_core::log_transitions::{FailureLog, KeyedFailureLatch};
use temps_core::url_validation::{
    resolve_and_validate_domain, validate_outbound_ip, UrlValidationError,
};
use temps_core::{Job, JobQueue, JobReceiver, QueueError, StatusCheckCompletedJob};
use temps_entities::{
    cloudflare_access_providers, deployment_containers, deployments, environment_domains,
    environments, project_custom_domains, projects, status_checks, status_monitors,
};
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use super::types::{normalize_check_interval_seconds, validate_check_path, StatusPageError};

/// Grace period before the scheduler runs its first health check cycle.
///
/// In split mode (`temps proxy` + `temps serve --role=console`) these are
/// independent OS processes with no startup handshake between them, and even
/// in single-process mode the proxy's listener setup (route/project-change
/// listeners, admin gate, DB connections) can still be in flight when the
/// console's plugins finish registering. A check that fires the instant this
/// scheduler starts races the proxy socket bind: `check_monitor` gets a
/// connection-refused, which is treated as a definitive "container is down"
/// and reported as `major_outage` with no retry (see the `is_connect()`
/// branch below). This delay absorbs that boot window so the first cycle
/// observes the platform in its normal running state; every cycle after the
/// first runs on the regular interval.
const STARTUP_GRACE_PERIOD: Duration = Duration::from_secs(20);

/// How often the scheduler looks for monitors that have come due.
///
/// This is the *sweep* cadence, not the per-monitor check cadence — each
/// monitor is probed on its own `check_interval_seconds`. The sweep only has
/// to be fine-grained enough that a monitor is not checked much later than it
/// asked for; at 15s a 30s monitor drifts by at most half an interval. The
/// sweep itself is a single indexed query that usually returns nothing, so
/// running it four times a minute costs far less than the old design, which
/// probed every active monitor once a minute.
const SCHEDULER_SWEEP_INTERVAL: Duration = Duration::from_secs(15);
const MAX_RETRIES: u32 = 3;
const INITIAL_DELAY_MS: u64 = 100;
const MAX_DELAY_MS: u64 = 2000;

#[derive(Debug, thiserror::Error)]
enum ProbeSendError {
    #[error("request failed after {attempts} attempts: {source}")]
    Request {
        attempts: u32,
        #[source]
        source: reqwest::Error,
    },
    #[error("request timed out after {attempts} attempts")]
    Timeout { attempts: u32 },
}

async fn send_with_timeout_retries(
    client: &reqwest::Client,
    url: &str,
    request_timeout: Duration,
) -> Result<(reqwest::Response, u32), ProbeSendError> {
    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay = std::cmp::min(INITIAL_DELAY_MS * (2_u64.pow(attempt - 1)), MAX_DELAY_MS);
            sleep(Duration::from_millis(delay)).await;
        }

        match timeout(request_timeout, client.get(url).send()).await {
            Ok(Ok(response)) => return Ok((response, attempt + 1)),
            Ok(Err(source)) if source.is_timeout() && attempt < MAX_RETRIES => continue,
            Ok(Err(source)) => {
                return Err(ProbeSendError::Request {
                    attempts: attempt + 1,
                    source,
                });
            }
            Err(_) if attempt < MAX_RETRIES => continue,
            Err(_) => {
                return Err(ProbeSendError::Timeout {
                    attempts: attempt + 1,
                });
            }
        }
    }

    Err(ProbeSendError::Timeout {
        attempts: MAX_RETRIES + 1,
    })
}

fn probe_url(
    public_url: &str,
    monitor_type: &str,
    check_path: Option<&str>,
) -> Result<String, StatusPageError> {
    let base = public_url.trim_end_matches('/');
    match check_path {
        Some(path) => {
            validate_check_path(path)?;
            if path == "/" {
                Ok(base.to_string())
            } else {
                Ok(format!("{base}{path}"))
            }
        }
        None if monitor_type == "health" => Ok(format!("{base}/health")),
        None => Ok(public_url.to_string()),
    }
}

/// Turn a proxy bind address into an address the console process can connect
/// to. Wildcard addresses describe where a listener binds, but are not valid
/// destinations, so retain the configured address family and use its loopback
/// address instead.
fn local_proxy_destination(listener: &str) -> Result<SocketAddr, StatusPageError> {
    let address = listener.parse::<SocketAddr>().map_err(|error| {
        StatusPageError::InvalidRequest(format!(
            "Configured proxy listener '{listener}' is not a socket address: {error}"
        ))
    })?;

    let ip = match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Ok(SocketAddr::new(ip, address.port()))
}

/// Build a client whose URL, Host header, and TLS identity remain the public
/// deployment hostname while the TCP connection is pinned to the local proxy.
/// Disabling inherited proxy settings is essential: otherwise HTTP_PROXY can
/// bypass the pinned resolver and send an internal availability probe outside
/// the installation.
fn local_proxy_client(
    logical_url: &str,
    listener: &str,
) -> Result<reqwest::Client, StatusPageError> {
    local_proxy_client_with_roots(logical_url, listener, &[])
}

fn local_proxy_client_with_roots(
    logical_url: &str,
    listener: &str,
    additional_roots: &[reqwest::Certificate],
) -> Result<reqwest::Client, StatusPageError> {
    let url = reqwest::Url::parse(logical_url).map_err(|error| {
        StatusPageError::InvalidRequest(format!(
            "Generated deployment URL '{logical_url}' is invalid: {error}"
        ))
    })?;
    let hostname = url.host_str().ok_or_else(|| {
        StatusPageError::InvalidRequest(format!(
            "Generated deployment URL '{logical_url}' has no hostname"
        ))
    })?;
    let destination = local_proxy_destination(listener)?;

    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Temps-Status-Monitor/1.0")
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve(hostname, destination);
    for root in additional_roots {
        builder = builder.add_root_certificate(root.clone());
    }
    builder
        .build()
        .map_err(|source| StatusPageError::HttpClientBuild { source })
}

/// Where a non-local (public URL) probe may connect, decided at probe time.
enum PublicProbeTarget {
    /// Every resolved address passed the outbound policy; the client is pinned
    /// to exactly those addresses.
    Pinned(reqwest::Client),
    /// The host is (or resolves to) an address outside the outbound policy:
    /// loopback, RFC 1918, link-local/metadata, etc., and not trusted.
    NonPublic(String),
    /// The host did not resolve.
    Unresolved(String),
}

/// Re-resolve and validate the probe host at probe time, and build a client
/// pinned to the validated addresses.
///
/// The probed hostname is validated nowhere else: a domain whose DNS later
/// points at `127.0.0.1`, an RFC 1918 address or `169.254.169.254` would
/// otherwise turn every periodic probe into a request to that internal
/// address. Resolving once, validating every address with the shared outbound
/// policy (which honours the operator's trusted private networks), and
/// pinning the client to those addresses means a rebinding answer cannot slip
/// in between the check and the connect. Inherited proxy settings are
/// disabled for the same reason (a proxy would do its own resolution), and
/// redirects stay unfollowed.
async fn public_probe_target(url: &str) -> Result<PublicProbeTarget, StatusPageError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| {
        StatusPageError::InvalidRequest(format!("Health URL '{url}' is invalid: {error}"))
    })?;
    let port = parsed.port_or_known_default().ok_or_else(|| {
        StatusPageError::InvalidRequest(format!("Health URL '{url}' has no usable port"))
    })?;
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Temps-Status-Monitor/1.0")
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            if let Err(e) = validate_outbound_ip(IpAddr::V4(ip)) {
                return Ok(PublicProbeTarget::NonPublic(e.to_string()));
            }
        }
        Some(url::Host::Ipv6(ip)) => {
            if let Err(e) = validate_outbound_ip(IpAddr::V6(ip)) {
                return Ok(PublicProbeTarget::NonPublic(e.to_string()));
            }
        }
        Some(url::Host::Domain(domain)) => match resolve_and_validate_domain(domain, port).await {
            Ok(addresses) => builder = builder.resolve_to_addrs(domain, &addresses),
            Err(UrlValidationError::DnsResolutionFailed(reason)) => {
                return Ok(PublicProbeTarget::Unresolved(reason));
            }
            Err(e) => return Ok(PublicProbeTarget::NonPublic(e.to_string())),
        },
        None => {
            return Err(StatusPageError::InvalidRequest(format!(
                "Health URL '{url}' has no host"
            )));
        }
    }
    builder
        .build()
        .map(PublicProbeTarget::Pinned)
        .map_err(|source| StatusPageError::HttpClientBuild { source })
}

fn should_route_probe_locally(is_managed: bool, uses_external_url: bool) -> bool {
    is_managed && !uses_external_url
}

fn local_listener_for_url<'a>(
    logical_url: &str,
    http_listener: &'a str,
    tls_listener: Option<&'a str>,
) -> Option<&'a str> {
    if logical_url.starts_with("https://") {
        tls_listener
    } else {
        Some(http_listener)
    }
}

const PROXY_HTTPS_REDIRECT_HEADER: &str = "x-temps-proxy-https-redirect";
const PROXY_PROBE_CAPABILITY_HEADER: &str = "x-temps-proxy-probe-capable";

/// Where a probe's answer came from, which decides what in it is evidence
/// of the gate (ADR 065 D10).
#[derive(Debug, Clone, Copy)]
enum ProbeOrigin<'a> {
    /// Through this server's own proxy, which drops `x-temps-gate` from
    /// every app response: the header can only be the gate's.
    LocalProxy,
    /// Straight to the public address. Anything may answer there, so the
    /// header proves nothing; only a redirect to the sign-in of the
    /// Cloudflare Access team the address is bound to counts.
    Direct { access_team: Option<&'a str> },
}

/// What a monitor reports when the address it checks is behind sign-in
/// (ADR 065 D10): the gate answered, so the address is up, but the app's
/// own health is out of the monitor's sight.
///
/// Through the local proxy, only an answer that is the gate doing its job
/// counts: the Temps sign-in or a Cloudflare Access refusal at the origin.
/// `unavailable` means the gate itself cannot run, so the address is down,
/// and an unknown value is not trusted either.
fn sign_in_note(
    origin: ProbeOrigin<'_>,
    status: u16,
    gate_header: Option<&str>,
    location: Option<&str>,
) -> Option<String> {
    let gated = match origin {
        ProbeOrigin::LocalProxy => gate_header.is_some_and(|value| {
            value.eq_ignore_ascii_case(temps_core::gate::GATE_SIGN_IN)
                || value.eq_ignore_ascii_case(temps_core::gate::GATE_ACCESS)
        }),
        ProbeOrigin::Direct { access_team } => {
            matches!(status, 302 | 303)
                && access_team
                    .is_some_and(|team| temps_core::gate_access::is_team_sign_in(location, team))
        }
    };
    gated.then(|| {
        "Behind sign-in: this address only lets in people who sign in, so the monitor sees \
         the sign-in, not the app. Temps offers no way past it. Container health checks \
         (restarts, out-of-memory kills, exits) watch the app itself."
            .to_string()
    })
}

/// [`sign_in_note`] for an answer that came through this server's own proxy:
/// the first answer of a local probe, and the answer to its HTTPS follow-up,
/// where a Cloudflare Access refusal at the origin shows up when the address
/// forces HTTPS.
fn local_sign_in_note(response: &reqwest::Response) -> Option<String> {
    sign_in_note(
        ProbeOrigin::LocalProxy,
        response.status().as_u16(),
        response
            .headers()
            .get(temps_core::gate::GATE_HEADER)
            .and_then(|value| value.to_str().ok()),
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
    )
}

/// The team of the Cloudflare Access account the address a direct probe
/// asked for is bound to, looked up only when the answer redirects to some
/// Access sign-in: every other direct answer needs no lookup.
async fn direct_access_team(
    db: &DatabaseConnection,
    monitor_id: i32,
    url: &str,
    status: u16,
    location: Option<&str>,
) -> Option<String> {
    if !matches!(status, 302 | 303)
        || !location.is_some_and(|l| l.contains(temps_core::gate_access::TEAM_DOMAIN_SUFFIX))
    {
        return None;
    }
    let host = reqwest::Url::parse(url)
        .ok()?
        .host_str()?
        .to_ascii_lowercase();
    match access_team_of_host(db, &host).await {
        Ok(team) => team,
        Err(error) => {
            warn!(
                monitor_id,
                host = %host,
                %error,
                "Could not read the Cloudflare Access account of the probed address"
            );
            None
        }
    }
}

/// The team domain of the Cloudflare Access account `host` is bound to, when
/// it is a signed-in address behind Access.
async fn access_team_of_host(
    db: &DatabaseConnection,
    host: &str,
) -> Result<Option<String>, sea_orm::DbErr> {
    use temps_entities::audience::{GATE_CLOUDFLARE_ACCESS, SIGNED_IN};
    let custom = project_custom_domains::Entity::find()
        .filter(project_custom_domains::Column::Domain.eq(host))
        .filter(project_custom_domains::Column::Audience.eq(SIGNED_IN))
        .filter(project_custom_domains::Column::Gate.eq(GATE_CLOUDFLARE_ACCESS))
        .one(db)
        .await?
        .and_then(|d| d.gate_access_provider_id);
    let provider_id = match custom {
        Some(id) => Some(id),
        None => environment_domains::Entity::find()
            .filter(environment_domains::Column::Domain.eq(host))
            .filter(environment_domains::Column::Audience.eq(SIGNED_IN))
            .filter(environment_domains::Column::Gate.eq(GATE_CLOUDFLARE_ACCESS))
            .one(db)
            .await?
            .and_then(|d| d.gate_access_provider_id),
    };
    let Some(provider_id) = provider_id else {
        return Ok(None);
    };
    Ok(cloudflare_access_providers::Entity::find_by_id(provider_id)
        .one(db)
        .await?
        .map(|p| p.team_domain))
}

fn proxy_supports_probe_markers(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(PROXY_PROBE_CAPABILITY_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some("1")
}

fn same_host_https_redirect(requested_url: &str, response: &reqwest::Response) -> Option<String> {
    if !response.status().is_redirection() {
        return None;
    }
    let requested = reqwest::Url::parse(requested_url).ok()?;
    if requested.scheme() != "http" {
        return None;
    }
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    let target = requested.join(location).ok()?;
    (target.scheme() == "https"
        && target.host_str() == requested.host_str()
        && target.port_or_known_default() == Some(443))
    .then(|| target.to_string())
}

fn proxy_https_redirect(requested_url: &str, response: &reqwest::Response) -> Option<String> {
    (response
        .headers()
        .get(PROXY_HTTPS_REDIRECT_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some("1"))
    .then(|| same_host_https_redirect(requested_url, response))
    .flatten()
}

fn local_https_follow_up_url(
    requested_url: &str,
    response: &reqwest::Response,
) -> Option<(String, bool)> {
    proxy_https_redirect(requested_url, response)
        .map(|url| (url, false))
        .or_else(|| {
            (!proxy_supports_probe_markers(response))
                .then(|| same_host_https_redirect(requested_url, response))
                .flatten()
                .map(|url| (url, true))
        })
}

fn https_follow_up_failure_status(legacy_ambiguous_redirect: bool) -> &'static str {
    if legacy_ambiguous_redirect {
        "degraded"
    } else {
        "major_outage"
    }
}

#[derive(Clone)]
struct MonitorProbeSnapshot {
    monitor_id: i32,
    deployment_id: i32,
    monitor_updated_at: temps_core::UtcDateTime,
}

/// Service for performing health checks on monitored environments
/// Monitors whose most recent recorded check was not `operational`, keyed by
/// monitor ID.
///
/// The monitored application is the user's workload: it being down is the
/// outcome the monitor exists to record (in `status_checks`, outages and
/// notifications), not a Temps fault. The server log gets one WARN when a
/// monitor starts failing, an hourly reminder while it stays down, and an
/// INFO when it recovers — not a line on every check of every down monitor.
///
/// Process-wide because probes run in detached tasks that do not carry the
/// service instance. Only failing monitors are stored, so it is bounded by
/// the number of monitors.
pub(super) fn clear_monitor_log_state(monitor_id: i32) {
    MONITOR_DOWN.record_success(&monitor_id);
    MONITOR_CHECK_ERRORS.record_success(&monitor_id);
}

static MONITOR_DOWN: LazyLock<KeyedFailureLatch<i32>> = LazyLock::new(KeyedFailureLatch::default);

/// Monitors whose check could not run at all (see [`report_check_error`]).
static MONITOR_CHECK_ERRORS: LazyLock<KeyedFailureLatch<i32>> =
    LazyLock::new(KeyedFailureLatch::default);

/// Whole-sweep failures (the due-monitor query itself failed).
static SWEEP_FAILURES: temps_core::log_transitions::FailureLatch =
    temps_core::log_transitions::FailureLatch::new(
        temps_core::log_transitions::DEFAULT_REMINDER_INTERVAL,
    );

/// Record the outcome of a persisted check for the server log.
fn report_check_outcome(
    latch: &KeyedFailureLatch<i32>,
    monitor_id: i32,
    status: &str,
    error_message: Option<&str>,
) -> Option<FailureLog> {
    if status == NO_ADDRESS_STATUS {
        // Nothing was probed (ADR 068 D4): neither a failure nor a recovery.
        return None;
    }
    if status == "operational" {
        if let Some(failures) = latch.record_success(&monitor_id) {
            info!(
                monitor_id,
                previous_failed_checks = failures,
                "Monitor is operational again"
            );
        }
        return None;
    }
    let outcome = latch.record_failure_bounded(monitor_id, 4096);
    let error_message = error_message.unwrap_or("none");
    match outcome {
        FailureLog::Started => warn!(
            monitor_id,
            status,
            error_message,
            "Monitor check is failing; the monitored application is not healthy"
        ),
        FailureLog::Reminder { consecutive } => warn!(
            monitor_id,
            status,
            error_message,
            consecutive_failed_checks = consecutive,
            "Monitor is still failing"
        ),
        FailureLog::Suppressed { consecutive } => debug!(
            monitor_id,
            status,
            error_message,
            consecutive_failed_checks = consecutive,
            "Monitor check is still failing"
        ),
    }
    Some(outcome)
}

/// Log a check that could not run. A monitor without an environment, or
/// whose environment is gone, is a configuration state of that monitor
/// (WARN); anything else is a Temps fault (ERROR). Either way it is logged
/// once per monitor until a check runs again, not on every interval.
fn report_check_error(
    latch: &KeyedFailureLatch<i32>,
    monitor_id: i32,
    error: &StatusPageError,
) -> FailureLog {
    let outcome = latch.record_failure_bounded(monitor_id, 4096);
    let is_configuration = matches!(
        error,
        StatusPageError::NotFound | StatusPageError::InvalidRequest(_)
    );
    match (outcome.should_log(), is_configuration) {
        (true, true) => warn!(
            monitor_id,
            consecutive_failures = outcome.consecutive(),
            "Monitor cannot be checked: {}",
            error
        ),
        (true, false) => error!(
            monitor_id,
            consecutive_failures = outcome.consecutive(),
            "Health check failed: {:?}",
            error
        ),
        (false, _) => debug!(
            monitor_id,
            consecutive_failures = outcome.consecutive(),
            "Health check failed: {:?}",
            error
        ),
    }
    outcome
}

pub struct HealthCheckService {
    db: Arc<DatabaseConnection>,
    http_client: reqwest::Client,
    config_service: Arc<ConfigService>,
    job_queue: Arc<dyn JobQueue>,
    /// ADR 068 D4: which address to probe, if any. Empty without the
    /// ingress plugin, where every monitor probes as before.
    environment_addresses: Option<Arc<EnvironmentAddressSlot>>,
}

/// What a monitor records when its environment has no address anyone can
/// reach (ADR 068 D4): it does not probe, and it is never `down`.
pub const NO_ADDRESS_STATUS: &str = temps_core::environment_address::NO_ADDRESS_CHECK_STATUS;
pub const NO_ADDRESS_MESSAGE: &str = "Nothing to check: nobody can reach this environment yet";

/// The URL a monitor probes for the environment's primary address, and
/// whether it is a public address to dial (not the local proxy). `None`
/// keeps today's URL: the generated host, a private address, or no answer.
fn primary_probe_url(address: Option<&EnvironmentAddress>) -> Option<String> {
    let primary = address?.primary.as_ref()?;
    match primary.kind {
        AddressKind::CustomDomain | AddressKind::TunnelHost => Some(https_url(&primary.address)),
        AddressKind::Generated | AddressKind::Private => None,
    }
}

impl HealthCheckService {
    /// Match deployment-readiness semantics: a redirect is a valid public
    /// application response (login/setup redirects are common), while 4xx/5xx
    /// indicate that the configured application URL is not usable.
    fn is_operational_http_status(status: reqwest::StatusCode) -> bool {
        status.is_success() || status.is_redirection()
    }

    /// Create a new HealthCheckService with mandatory ConfigService and JobQueue
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        job_queue: Arc<dyn JobQueue>,
    ) -> Result<Self, StatusPageError> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Temps-Status-Monitor/1.0")
            // The application controls redirect targets. Following them would
            // turn periodic monitoring into a blind SSRF primitive against
            // loopback/private/link-local services, and would also prevent us
            // from classifying the original 3xx as a healthy app response.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| StatusPageError::HttpClientBuild { source })?;

        Ok(Self {
            db,
            http_client,
            config_service,
            job_queue,
            environment_addresses: None,
        })
    }

    /// Probe each environment's primary address (ADR 068 D4).
    pub fn with_environment_addresses(mut self, slot: Arc<EnvironmentAddressSlot>) -> Self {
        self.environment_addresses = Some(slot);
        self
    }

    fn addresses(&self) -> Option<Arc<dyn EnvironmentAddresses>> {
        self.environment_addresses.as_ref().and_then(|s| s.get())
    }

    /// Resolve a monitor's configured interval into the value the scheduler
    /// actually uses.
    ///
    /// Shares [`normalize_check_interval_seconds`] with the write boundary in
    /// `MonitorService::create_monitor`, so a stored row and the scheduler can
    /// never disagree about what "every N seconds" means. This remains a
    /// second line of defence for rows written before that validation existed
    /// (or by a migration/import).
    fn effective_check_interval_secs(check_interval_seconds: i32) -> i64 {
        i64::from(normalize_check_interval_seconds(check_interval_seconds))
    }

    /// Select the active monitors that are due at `now`.
    ///
    /// `next_check_at IS NULL` means the monitor has never been scheduled
    /// (rows predating the due column, or freshly created ones), so it is
    /// treated as due — nothing needs a backfill to start being checked.
    fn due_monitors_query(now: temps_core::UtcDateTime) -> Select<status_monitors::Entity> {
        status_monitors::Entity::find()
            .filter(status_monitors::Column::IsActive.eq(true))
            .filter(
                Condition::any()
                    .add(status_monitors::Column::NextCheckAt.is_null())
                    .add(status_monitors::Column::NextCheckAt.lte(now)),
            )
    }

    /// Stamp the monitor's next due time unconditionally.
    ///
    /// Used by the paths that probe a single known monitor outside the sweep
    /// (`check_monitors_for_environment`, the `MonitorCreated` reaction):
    /// there is no competing claimant there, but the probe still consumes the
    /// monitor's slot and must push the due time out.
    ///
    /// Deliberately written through `update_many` + `col_expr` rather than an
    /// `ActiveModel` save: `ActiveModelBehavior::before_save` bumps
    /// `updated_at`, and `persist_check_if_current` uses `updated_at` as the
    /// monitor's concurrency token. Saving an ActiveModel here would make
    /// every check invalidate its own result.
    ///
    /// Called *before* the probe runs so that a probe which panics, times out
    /// or exits early (paused deployment, missing environment) still yields
    /// the slot instead of being re-selected by the very next sweep.
    async fn schedule_next_check(db: &DatabaseConnection, monitor: &status_monitors::Model) {
        let interval = Self::effective_check_interval_secs(monitor.check_interval_seconds);
        let next_check_at = Utc::now() + chrono::Duration::seconds(interval);

        if let Err(error) = status_monitors::Entity::update_many()
            .col_expr(
                status_monitors::Column::NextCheckAt,
                Expr::value(next_check_at),
            )
            .filter(status_monitors::Column::Id.eq(monitor.id))
            .exec(db)
            .await
        {
            // Non-fatal: the monitor stays due and is retried next sweep.
            warn!(
                monitor_id = monitor.id,
                interval_secs = interval,
                error = %error,
                "Failed to stamp next_check_at for monitor; it will be re-selected next sweep"
            );
        }
    }

    /// Atomically claim a due monitor's slot.
    ///
    /// The sweep runs every [`SCHEDULER_SWEEP_INTERVAL`] and each cycle is
    /// spawned without awaiting the previous one, so two sweeps can overlap
    /// whenever a cycle takes longer than the sweep interval (easy: probes are
    /// bounded to 10 at a time and each can take up to 4 HTTP attempts with
    /// backoff). Stamping inside the probe therefore left the un-dispatched
    /// tail of a large due set unstamped and the next sweep probed it again.
    ///
    /// The fix is a compare-and-swap: the `UPDATE` carries the exact
    /// `next_check_at` value this sweep observed as a predicate, so the first
    /// sweep to reach a row wins (`rows_affected == 1`) and every other
    /// claimant sees `rows_affected == 0` and skips the monitor. Claiming
    /// happens for the whole selected set **before** any probe is dispatched,
    /// which is what makes the window airtight rather than merely smaller.
    ///
    /// Same `update_many` + `col_expr` shape as `schedule_next_check`, and for
    /// the same reason: `updated_at` is the OCC token `persist_check_if_current`
    /// reads, so an `ActiveModel` save here would invalidate every probe.
    ///
    /// Returns `true` when this caller owns the probe for this slot.
    async fn claim_due_monitor(db: &DatabaseConnection, monitor: &status_monitors::Model) -> bool {
        let interval = Self::effective_check_interval_secs(monitor.check_interval_seconds);
        let next_check_at = Utc::now() + chrono::Duration::seconds(interval);

        // CAS predicate on the observed value. NULL needs its own arm because
        // `= NULL` is never true in SQL.
        let observed = match monitor.next_check_at {
            Some(observed) => status_monitors::Column::NextCheckAt.eq(observed),
            None => status_monitors::Column::NextCheckAt.is_null(),
        };

        match status_monitors::Entity::update_many()
            .col_expr(
                status_monitors::Column::NextCheckAt,
                Expr::value(next_check_at),
            )
            .filter(status_monitors::Column::Id.eq(monitor.id))
            .filter(observed)
            .exec(db)
            .await
        {
            Ok(result) if result.rows_affected == 1 => true,
            Ok(_) => {
                debug!(
                    monitor_id = monitor.id,
                    "Monitor already claimed by an overlapping sweep; skipping"
                );
                false
            }
            Err(error) => {
                // Non-fatal: the monitor stays due and is retried next sweep.
                // Not probing is the safe choice — probing without a stamp is
                // exactly the duplicate-probe bug this guards against.
                warn!(
                    monitor_id = monitor.id,
                    interval_secs = interval,
                    error = %error,
                    "Failed to claim monitor slot; it will be re-selected next sweep"
                );
                false
            }
        }
    }

    /// Run health checks for every monitor that is currently due
    pub async fn run_all_checks(&self) -> Result<(), StatusPageError> {
        debug!("Starting health check cycle");

        // Single query: join monitors with environments to skip on-demand ones.
        // Health checks go through the proxy, which resets the idle timer and
        // would prevent scale-to-zero from ever triggering.
        let monitors_with_envs = Self::due_monitors_query(Utc::now())
            .find_also_related(environments::Entity)
            .all(self.db.as_ref())
            .await?;

        let total_monitors = monitors_with_envs.len();
        debug!("Found {} due monitors to check", total_monitors);

        // Claim every selected row *before* dispatching any probe.
        //
        // On-demand environments are claimed too even though they are never
        // probed (a health check through the proxy resets the idle timer and
        // would defeat scale-to-zero). Filtering them out without stamping
        // left them permanently due, so the sweep re-fetched the entire
        // on-demand set four times a minute — the exact full-scan cost this
        // change exists to remove. Stamping them costs one write per interval
        // and keeps them out of the next sweep's result set.
        let mut claimed_with_envs = Vec::with_capacity(monitors_with_envs.len());
        for (monitor, env) in monitors_with_envs {
            if Self::claim_due_monitor(self.db.as_ref(), &monitor).await {
                claimed_with_envs.push((monitor, env));
            }
        }

        let claimed_monitors = claimed_with_envs.len();
        let filtered_monitors: Vec<_> = Self::filter_on_demand_monitors(claimed_with_envs);

        debug!(
            "Running checks for {} monitors ({} skipped as on-demand, {} claimed by another sweep)",
            filtered_monitors.len(),
            claimed_monitors - filtered_monitors.len(),
            total_monitors - claimed_monitors
        );

        // Run checks concurrently with a limit
        let semaphore = Arc::new(tokio::sync::Semaphore::new(10)); // Limit concurrent checks
        let mut tasks = Vec::new();

        for monitor in filtered_monitors {
            let db = self.db.clone();
            let config_service = self.config_service.clone();
            let job_queue = self.job_queue.clone();
            let addresses = self.addresses();
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    error!(
                        ?error,
                        "Health-check concurrency limiter closed unexpectedly"
                    );
                    break;
                }
            };

            let monitor_id = monitor.id;
            let task = tokio::spawn(async move {
                let _permit = permit; // Hold permit until task completes
                match Self::check_monitor_with(db, config_service, monitor, job_queue, addresses)
                    .await
                {
                    Ok(()) => {
                        if MONITOR_CHECK_ERRORS.record_success(&monitor_id).is_some() {
                            info!(monitor_id, "Monitor can be checked again");
                        }
                    }
                    Err(e) => {
                        report_check_error(&MONITOR_CHECK_ERRORS, monitor_id, &e);
                    }
                }
            });

            tasks.push(task);
        }

        // Wait for all checks to complete
        for task in tasks {
            if let Err(e) = task.await {
                error!("Task failed: {:?}", e);
            }
        }

        debug!("Health check cycle completed");
        Ok(())
    }

    /// Recompute all active monitors for one deployed environment.
    ///
    /// Deployment success calls this after updating the monitor's health path,
    /// avoiding up to a minute of stale Down/Unknown state while preserving the
    /// periodic scheduler's scale-to-zero exclusion for on-demand environments.
    pub async fn check_monitors_for_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<usize, StatusPageError> {
        let monitors_with_envs = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .filter(status_monitors::Column::EnvironmentId.eq(Some(environment_id)))
            .filter(status_monitors::Column::IsActive.eq(true))
            .find_also_related(environments::Entity)
            .all(self.db.as_ref())
            .await?;
        let monitors = Self::filter_on_demand_monitors(monitors_with_envs);
        let monitor_count = monitors.len();

        for monitor in monitors {
            // Out-of-band probe: no sweep is racing for this row, but the
            // probe still consumes the monitor's slot, so push the due time
            // out before running it.
            Self::schedule_next_check(self.db.as_ref(), &monitor).await;
            Self::check_monitor_with(
                self.db.clone(),
                self.config_service.clone(),
                monitor,
                self.job_queue.clone(),
                self.addresses(),
            )
            .await?;
        }

        Ok(monitor_count)
    }

    /// Check a single monitor, without knowing environment addresses.
    #[cfg(test)]
    async fn check_monitor(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        monitor: status_monitors::Model,
        job_queue: Arc<dyn JobQueue>,
    ) -> Result<(), StatusPageError> {
        Self::check_monitor_with(db, config_service, monitor, job_queue, None).await
    }

    /// Check a single monitor
    async fn check_monitor_with(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        monitor: status_monitors::Model,
        job_queue: Arc<dyn JobQueue>,
        addresses: Option<Arc<dyn EnvironmentAddresses>>,
    ) -> Result<(), StatusPageError> {
        // NOTE: the monitor's slot is claimed by the *caller*, before this is
        // dispatched — `run_all_checks` via `claim_due_monitor` (a CAS that
        // also decides who probes), the single-monitor paths via
        // `schedule_next_check`. Stamping here instead would have to happen
        // after the probe was already in flight, which is what allowed two
        // overlapping sweeps to probe the same monitor.
        //
        // The stamp is unconditional on the caller side so that every early
        // return below (no environment, no current deployment, paused
        // deployment) still yields the slot instead of leaving `next_check_at`
        // in the past and being re-selected on every single sweep.

        // Check if environment_id is set
        // Logged (once per monitor) by the caller.
        let env_id = monitor.environment_id.ok_or_else(|| {
            StatusPageError::InvalidRequest(format!("monitor {} has no environment_id", monitor.id))
        })?;

        debug!("Checking monitor {} for environment {}", monitor.id, env_id);

        // Get the environment to find its deployment URL
        let environment = environments::Entity::find_by_id(env_id)
            .one(db.as_ref())
            .await?
            .ok_or_else(|| StatusPageError::NotFound)?;
        let Some(current_deployment_id) = environment.current_deployment_id else {
            // A never-deployed environment is a normal state, and this runs on
            // every sweep for each of its monitors.
            debug!(
                "Skipping monitor {}: environment {} has no current deployment",
                monitor.id, env_id
            );
            return Ok(());
        };

        // Skip monitors whose deployment was intentionally paused by the
        // user. This is a live read (not a value cached earlier in the
        // caller) so it also covers the immediate check fired right after a
        // monitor is created, and it can't be stale relative to
        // `pause_deployment`, which persists `state = "paused"` before it
        // stops any containers. This is only the cheap early exit — it
        // avoids the HTTP round trip in the common case, but a pause can
        // still land *during* the request below (which may take several
        // seconds across retries), so `record_check` re-checks live again
        // right before persisting any outcome.
        let deployment = deployments::Entity::find_by_id(current_deployment_id)
            .one(db.as_ref())
            .await?;
        let Some(deployment) = deployment else {
            return Ok(());
        };
        if deployment.state == "paused" {
            debug!(
                "Skipping monitor {} for paused deployment {}",
                monitor.id, current_deployment_id
            );
            return Ok(());
        }
        if deployment.static_dir_location.is_none()
            && Self::is_portless_deployment(db.as_ref(), current_deployment_id).await?
        {
            debug!(
                monitor_id = monitor.id,
                deployment_id = current_deployment_id,
                "Retiring HTTP monitor for background worker"
            );
            let already_retired = status_checks::Entity::find()
                .filter(status_checks::Column::MonitorId.eq(monitor.id))
                .order_by_desc(status_checks::Column::CheckedAt)
                .order_by_desc(status_checks::Column::Id)
                .one(db.as_ref())
                .await?
                .is_some_and(|check| {
                    check.status == "unknown"
                        && check.error_message.as_deref()
                            == Some("HTTP monitoring does not apply to a background worker")
                });
            if already_retired {
                return Ok(());
            }
            // This is retirement of an inapplicable HTTP monitor, not a successful
            // HTTP check or a runtime worker probe. The outage service rechecks the
            // current deployment and idempotently retires its old incidents/alarms.
            job_queue
                .send(Job::StatusCheckCompleted(StatusCheckCompletedJob {
                    monitor_id: monitor.id,
                    status: "not_applicable".into(),
                    error_message: None,
                }))
                .await
                .map_err(|error| {
                    StatusPageError::Internal(format!(
                        "Cannot retire HTTP monitor {}: {error}",
                        monitor.id
                    ))
                })?;
            return Ok(());
        }
        let probe = MonitorProbeSnapshot {
            monitor_id: monitor.id,
            deployment_id: current_deployment_id,
            monitor_updated_at: monitor.updated_at,
        };

        // ADR 068 D4: probe the environment's primary address when it has a
        // reachable one; otherwise probe exactly as before. `no_address` only
        // replaces the one failure that means nothing was ever set up: the
        // target hostname does not resolve and the environment has no
        // address at all. Without an answer, probe as before.
        let address = match &addresses {
            Some(addresses) => match addresses.address(environment.project_id, env_id).await {
                Ok(address) => Some(address),
                Err(error) => {
                    warn!(
                        monitor_id = monitor.id,
                        environment_id = env_id,
                        %error,
                        "Could not tell the primary address; probing the generated URL"
                    );
                    None
                }
            },
            None => None,
        };
        let no_address = address
            .as_ref()
            .is_some_and(EnvironmentAddress::has_no_address);
        let primary_url = primary_probe_url(address.as_ref());

        // IMPORTANT: Always use the public URL for health checks
        // This ensures we're testing the actual user-facing endpoint, not internal container networking
        let public_url = match primary_url {
            Some(url) => Ok((url, true)),
            None => {
                config_service
                    .get_deployment_url_by_slug_with_source(&environment.subdomain)
                    .await
            }
        };
        let (health_url, uses_external_url) = match public_url {
            Ok((public_url, uses_external_url)) => {
                debug!("Using public URL for health check: {}", public_url);
                // Use custom check_path if set, otherwise fall back to monitor_type logic.
                // Defense-in-depth: re-validate the stored path at use time so that any
                // rows written before write-time validation was added (or written by a
                // future migration/import path) cannot inject a manipulated URL.
                let health_url = match probe_url(
                    &public_url,
                    &monitor.monitor_type,
                    monitor.check_path.as_deref(),
                ) {
                    Ok(url) => url,
                    Err(e) => {
                        warn!(
                            monitor_id = monitor.id,
                            error = %e,
                            "Stored check_path failed validation; falling back to default URL"
                        );
                        public_url
                    }
                };
                (health_url, uses_external_url)
            }
            Err(e) => {
                let e = format!("{e:?}");
                error!(
                    "Failed to get public URL for deployment {}: {}",
                    environment.subdomain, e
                );

                // Record check as failed due to configuration error
                Self::record_check(
                    &db,
                    probe.clone(),
                    "degraded".to_string(),
                    None,
                    Some(format!("Failed to determine public URL: {e}")),
                    &job_queue,
                )
                .await?;

                return Ok(());
            }
        };

        // Probes that do not go through the local proxy dial the public URL.
        // Its host is re-resolved and validated here, and the client pinned
        // to the validated addresses, so it cannot be steered at an internal
        // address. A deployment hostname that resolves to a non-public address
        // (a LAN or local install whose wildcard DNS points at this server) is
        // served by the Temps proxy anyway, so it is probed through the local
        // listener -- never by dialing the resolved internal address.
        let mut pinned_public_client = None;
        if !should_route_probe_locally(monitor.is_managed, uses_external_url) {
            match public_probe_target(&health_url).await {
                Ok(PublicProbeTarget::Pinned(client)) => pinned_public_client = Some(client),
                Ok(PublicProbeTarget::NonPublic(reason)) => {
                    debug!(
                        monitor_id = monitor.id,
                        logical_url = %health_url,
                        %reason,
                        "Health URL does not resolve to an allowed public address; probing through the local proxy"
                    );
                }
                Ok(PublicProbeTarget::Unresolved(reason)) => {
                    // A name that does not resolve for an environment nobody
                    // was ever given a way into is not an outage: there is
                    // nothing to check yet. Any address that was once set up
                    // keeps the outage.
                    let (status, message) = if no_address {
                        debug!(
                            monitor_id = monitor.id,
                            environment_id = env_id,
                            %reason,
                            "Health URL does not resolve and the environment has no address"
                        );
                        (
                            NO_ADDRESS_STATUS.to_string(),
                            NO_ADDRESS_MESSAGE.to_string(),
                        )
                    } else {
                        (
                            "major_outage".to_string(),
                            format!("DNS resolution failed: {reason}"),
                        )
                    };
                    Self::record_check(&db, probe.clone(), status, None, Some(message), &job_queue)
                        .await?;
                    return Ok(());
                }
                Err(error) => {
                    Self::record_check(
                        &db,
                        probe.clone(),
                        "degraded".to_string(),
                        None,
                        Some(format!("Failed to configure health probe: {error}")),
                        &job_queue,
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        let (probe_client, is_local_probe, local_tls_listener) = if let Some(client) =
            pinned_public_client
        {
            (client, false, None)
        } else {
            let server_config = config_service.get_server_config();
            let Some(listener) = local_listener_for_url(
                &health_url,
                &server_config.address,
                server_config.tls_address.as_deref(),
            ) else {
                Self::record_check(
                        &db,
                        probe.clone(),
                        "degraded".to_string(),
                        None,
                        Some(
                            "Managed HTTPS health URL has no configured local TLS proxy listener; application health is unverified"
                                .to_string(),
                        ),
                        &job_queue,
                    )
                    .await?;
                return Ok(());
            };
            match local_proxy_client(&health_url, listener) {
                Ok(client) => {
                    debug!(
                        monitor_id = monitor.id,
                        logical_url = %health_url,
                        proxy_listener = %listener,
                        "Routing managed health check through the configured local proxy"
                    );
                    (client, true, server_config.tls_address.clone())
                }
                Err(error) => {
                    error!(
                        monitor_id = monitor.id,
                        logical_url = %health_url,
                        proxy_listener = %listener,
                        error = %error,
                        "Failed to configure local proxy health check"
                    );
                    Self::record_check(
                        &db,
                        probe.clone(),
                        "degraded".to_string(),
                        None,
                        Some(format!("Failed to configure local proxy probe: {error}")),
                        &job_queue,
                    )
                    .await?;
                    return Ok(());
                }
            }
        };

        debug!("Checking URL: {}", health_url);

        // Perform the health check with retry logic
        let start_time = std::time::Instant::now();
        let mut last_error = None;
        let mut total_response_time_ms = 0i32;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                // Exponential backoff: 100ms, 200ms, 400ms, 800ms (capped at 2000ms)
                let delay =
                    std::cmp::min(INITIAL_DELAY_MS * (2_u64.pow(attempt - 1)), MAX_DELAY_MS);
                debug!(
                    "Retrying health check for monitor {} (attempt {}/{}), waiting {}ms",
                    monitor.id, attempt, MAX_RETRIES, delay
                );
                sleep(Duration::from_millis(delay)).await;
            }

            let check_result = timeout(
                Duration::from_secs(10),
                probe_client.get(&health_url).send(),
            )
            .await;

            total_response_time_ms = start_time.elapsed().as_millis() as i32;

            match check_result {
                Ok(Ok(response)) => {
                    // ADR 065 D10: a signed-in address answers with its
                    // gate, never the app, and Temps offers no way past it.
                    let status = response.status().as_u16();
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|value| value.to_str().ok());
                    let gate_header = response
                        .headers()
                        .get(temps_core::gate::GATE_HEADER)
                        .and_then(|value| value.to_str().ok());
                    let access_team = if is_local_probe {
                        None
                    } else {
                        direct_access_team(&db, monitor.id, &health_url, status, location).await
                    };
                    let origin = if is_local_probe {
                        ProbeOrigin::LocalProxy
                    } else {
                        ProbeOrigin::Direct {
                            access_team: access_team.as_deref(),
                        }
                    };
                    if let Some(note) = sign_in_note(origin, status, gate_header, location) {
                        return Self::record_check(
                            &db,
                            probe.clone(),
                            "operational".to_string(),
                            Some(total_response_time_ms),
                            Some(note),
                            &job_queue,
                        )
                        .await;
                    }
                    // The HTTP proxy can emit its force-HTTPS redirect before
                    // resolving or contacting the application. Counting that
                    // response as healthy would let a dead upstream look
                    // operational. Follow only this tightly constrained
                    // same-host protocol upgrade through the configured local
                    // TLS listener. All application-controlled cross-host
                    // redirects remain unfollowed.
                    let response = if let Some((https_url, legacy_ambiguous_redirect)) =
                        is_local_probe
                            .then(|| local_https_follow_up_url(&health_url, &response))
                            .flatten()
                    {
                        let Some(tls_listener) = local_tls_listener.as_deref() else {
                            // A proxy-owned redirect is emitted before the app is
                            // contacted. Without a local TLS listener the probe is
                            // therefore inconclusive and must not report a dead app
                            // as operational.
                            return Self::record_check(
                                &db,
                                probe.clone(),
                                "degraded".to_string(),
                                Some(total_response_time_ms),
                                Some(
                                    "Local proxy requires HTTPS, but no local TLS listener is configured; application health is unverified"
                                        .to_string(),
                                ),
                                &job_queue,
                            )
                            .await;
                        };
                        let tls_client = match local_proxy_client(&https_url, tls_listener) {
                            Ok(client) => client,
                            Err(error) => {
                                return Self::record_check(
                                    &db,
                                    probe.clone(),
                                    "degraded".to_string(),
                                    Some(total_response_time_ms),
                                    Some(format!(
                                        "Failed to configure local HTTPS proxy probe: {error}"
                                    )),
                                    &job_queue,
                                )
                                .await;
                            }
                        };
                        let tls_result = send_with_timeout_retries(
                            &tls_client,
                            &https_url,
                            Duration::from_secs(10),
                        )
                        .await;
                        total_response_time_ms = start_time.elapsed().as_millis() as i32;
                        match tls_result {
                            Ok((response, tls_attempts)) => {
                                debug!(
                                    monitor_id = monitor.id,
                                    attempts = tls_attempts,
                                    "Local HTTPS proxy follow-up completed"
                                );
                                response
                            }
                            Err(error) => {
                                let status =
                                    https_follow_up_failure_status(legacy_ambiguous_redirect);
                                return Self::record_check(
                                    &db,
                                    probe.clone(),
                                    status.to_string(),
                                    Some(total_response_time_ms),
                                    Some(if legacy_ambiguous_redirect {
                                        format!(
                                            "Legacy local proxy redirect could not be verified through HTTPS: {error}"
                                        )
                                    } else {
                                        format!("Local HTTPS proxy probe failed: {error}")
                                    }),
                                    &job_queue,
                                )
                                .await;
                            }
                        }
                    } else {
                        response
                    };

                    // The follow-up came through the local proxy too, so its
                    // gate answer counts the same way as the first one's.
                    if is_local_probe {
                        if let Some(note) = local_sign_in_note(&response) {
                            return Self::record_check(
                                &db,
                                probe.clone(),
                                "operational".to_string(),
                                Some(total_response_time_ms),
                                Some(note),
                                &job_queue,
                            )
                            .await;
                        }
                    }

                    if is_local_probe
                        && response.status().is_redirection()
                        && !proxy_supports_probe_markers(&response)
                    {
                        return Self::record_check(
                            &db,
                            probe.clone(),
                            "degraded".to_string(),
                            Some(total_response_time_ms),
                            Some(
                                "Local proxy returned a redirect without probe capability metadata; application health is unverified during a mixed-version upgrade"
                                    .to_string(),
                            ),
                            &job_queue,
                        )
                        .await;
                    }
                    let status_code = response.status();

                    let status = if Self::is_operational_http_status(status_code) {
                        // Redirects are healthy (many apps redirect `/` to login or
                        // setup); client/server errors are not usable.
                        "operational"
                    } else if status_code.is_server_error() {
                        // For server errors, retry
                        if attempt < MAX_RETRIES {
                            last_error =
                                Some(format!("HTTP {} (attempt {})", status_code, attempt + 1));
                            continue;
                        }
                        "major_outage"
                    } else if status_code.is_client_error() {
                        "degraded"
                    } else {
                        "partial_outage"
                    };

                    debug!(
                        "Monitor {} check completed: {} ({}ms, {} attempts)",
                        monitor.id,
                        status,
                        total_response_time_ms,
                        attempt + 1
                    );

                    return Self::record_check(
                        &db,
                        probe.clone(),
                        status.to_string(),
                        Some(total_response_time_ms),
                        if status != "operational" {
                            Some(format!(
                                "HTTP {} (after {} attempts)",
                                status_code,
                                attempt + 1
                            ))
                        } else if attempt > 0 {
                            Some(format!("Succeeded after {} attempts", attempt + 1))
                        } else {
                            None
                        },
                        &job_queue,
                    )
                    .await;
                }
                Ok(Err(e)) => {
                    // Only retry on timeouts — connection refused means the container is down,
                    // retrying immediately just generates noise without any chance of success.
                    if e.is_timeout() && attempt < MAX_RETRIES {
                        last_error =
                            Some(format!("Request timeout: {} (attempt {})", e, attempt + 1));
                        continue;
                    }

                    // Non-retryable error or final attempt. The outcome is
                    // logged on state transitions by `record_check`.
                    debug!(
                        "Health check request failed for monitor {} after {} attempts: {:?}",
                        monitor.id,
                        attempt + 1,
                        e
                    );

                    let error_msg = if e.is_connect() {
                        "Connection failed"
                    } else if e.is_timeout() {
                        "Request timeout"
                    } else if e.is_redirect() {
                        "Too many redirects"
                    } else {
                        "Request failed"
                    };

                    return Self::record_check(
                        &db,
                        probe.clone(),
                        "major_outage".to_string(),
                        Some(total_response_time_ms),
                        Some(format!(
                            "{}: {} (after {} attempts)",
                            error_msg,
                            e,
                            attempt + 1
                        )),
                        &job_queue,
                    )
                    .await;
                }
                Err(_) => {
                    // Timeout - retry
                    if attempt < MAX_RETRIES {
                        last_error =
                            Some(format!("Health check timeout (attempt {})", attempt + 1));
                        continue;
                    }

                    debug!(
                        "Health check timeout for monitor {} after {} attempts",
                        monitor.id,
                        attempt + 1
                    );

                    return Self::record_check(
                        &db,
                        probe.clone(),
                        "major_outage".to_string(),
                        Some(10000), // Max timeout
                        Some(format!(
                            "Health check timeout after {} attempts",
                            attempt + 1
                        )),
                        &job_queue,
                    )
                    .await;
                }
            }
        }

        // Should not reach here, but handle it gracefully
        error!("Unexpected: exhausted retries for monitor {}", monitor.id);
        Self::record_check(
            &db,
            probe,
            "major_outage".to_string(),
            Some(total_response_time_ms),
            Some(last_error.unwrap_or_else(|| "Unknown error after retries".to_string())),
            &job_queue,
        )
        .await
    }

    /// Whether the deployment has containers but none declares a port.
    /// Static deployments are handled by the caller because they need no container.
    async fn is_portless_deployment(
        db: &DatabaseConnection,
        deployment_id: i32,
    ) -> Result<bool, sea_orm::DbErr> {
        // One aggregate row, regardless of replica count. Keep stopped workers
        // in this test so HTTP outages are never inferred from a missing port.
        let counts = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .select_only()
            .column_as(deployment_containers::Column::Id.count(), "container_count")
            .column_as(
                deployment_containers::Column::ContainerPort.count(),
                "port_count",
            )
            .into_tuple::<(i64, i64)>()
            .one(db)
            .await?;
        Ok(counts.is_some_and(|(containers, ports)| containers > 0 && ports == 0))
    }

    /// Record a check result in the database with retry logic and emit job for outage detection.
    ///
    /// Re-checks `deployment_id` for a pause live, right before persisting
    /// anything: `check_monitor`'s own paused guard only runs once, before
    /// the HTTP request, but that request (with retries) can take several
    /// seconds — long enough for a pause to land in between. Without this,
    /// an in-flight check that started against a running deployment could
    /// still record a "major_outage" for one that finished pausing seconds
    /// later.
    async fn record_check(
        db: &Arc<DatabaseConnection>,
        probe: MonitorProbeSnapshot,
        status: String,
        response_time_ms: Option<i32>,
        error_message: Option<String>,
        job_queue: &Arc<dyn JobQueue>,
    ) -> Result<(), StatusPageError> {
        let check = status_checks::ActiveModel {
            monitor_id: Set(probe.monitor_id),
            status: Set(status.clone()),
            response_time_ms: Set(response_time_ms),
            checked_at: Set(Utc::now()),
            error_message: Set(error_message.clone()),
            ..Default::default()
        };

        // Retry configuration for database operations
        const MAX_DB_RETRIES: u32 = 3;
        const INITIAL_DB_DELAY_MS: u64 = 50;

        let mut last_error = None;

        for attempt in 0..=MAX_DB_RETRIES {
            if attempt > 0 {
                let delay = INITIAL_DB_DELAY_MS * (2_u64.pow(attempt - 1));
                debug!(
                    "Retrying database insert for monitor {} (attempt {}/{}), waiting {}ms",
                    probe.monitor_id, attempt, MAX_DB_RETRIES, delay
                );
                sleep(Duration::from_millis(delay)).await;
            }

            match Self::persist_check_if_current(
                db,
                probe.monitor_id,
                probe.deployment_id,
                probe.monitor_updated_at,
                check.clone(),
            )
            .await
            {
                Ok(false) => {
                    debug!(
                        monitor_id = probe.monitor_id,
                        deployment_id = probe.deployment_id,
                        "Discarded health result because its deployment is no longer current or was paused"
                    );
                    return Ok(());
                }
                Ok(true) => {
                    if attempt > 0 {
                        debug!("Database insert succeeded after {} attempts", attempt + 1);
                    }
                    report_check_outcome(
                        &MONITOR_DOWN,
                        probe.monitor_id,
                        &status,
                        error_message.as_deref(),
                    );

                    // CRITICAL: Emit job for outage detection immediately after recording check
                    let job = Job::StatusCheckCompleted(StatusCheckCompletedJob {
                        monitor_id: probe.monitor_id,
                        status: status.clone(),
                        error_message: error_message.clone(),
                    });

                    if let Err(e) = job_queue.send(job).await {
                        error!(
                            "Failed to emit StatusCheckCompleted job for monitor {}: {:?}",
                            probe.monitor_id, e
                        );
                        // Don't fail the health check if job emission fails
                    }

                    return Ok(());
                }
                Err(e) => {
                    // Check if it's a transient error that we should retry
                    let should_retry = match &e {
                        sea_orm::DbErr::ConnectionAcquire(_) | sea_orm::DbErr::Conn(_) => true,
                        sea_orm::DbErr::Query(runtime_err) => {
                            let err_str = runtime_err.to_string();
                            err_str.contains("deadlock") || err_str.contains("timeout")
                        }
                        _ => false,
                    };

                    if should_retry && attempt < MAX_DB_RETRIES {
                        warn!(
                            "Database insert failed for monitor {} (attempt {}), will retry: {:?}",
                            probe.monitor_id,
                            attempt + 1,
                            e
                        );
                        last_error = Some(e);
                        continue;
                    }

                    // Non-retryable error or final attempt
                    error!(
                        "Failed to record check for monitor {} after {} attempts: {:?}",
                        probe.monitor_id,
                        attempt + 1,
                        e
                    );
                    return Err(StatusPageError::Database(e));
                }
            }
        }

        // Should not reach here, but handle it
        Err(StatusPageError::Database(last_error.unwrap_or_else(|| {
            sea_orm::DbErr::Custom("Failed after all retry attempts".to_string())
        })))
    }

    /// Commit a result only while the checked deployment is still the
    /// environment's current, unpaused deployment. The environment row lock
    /// gives deployment promotion and this insert one database ordering, and
    /// the monitor lock serializes concurrent probes for the same endpoint.
    async fn persist_check_if_current(
        db: &Arc<DatabaseConnection>,
        monitor_id: i32,
        deployment_id: i32,
        expected_monitor_updated_at: temps_core::UtcDateTime,
        check: status_checks::ActiveModel,
    ) -> Result<bool, sea_orm::DbErr> {
        let monitor_snapshot = status_monitors::Entity::find_by_id(monitor_id)
            .one(db.as_ref())
            .await?;
        let Some(environment_id) = monitor_snapshot.and_then(|monitor| monitor.environment_id)
        else {
            return Ok(false);
        };

        let transaction = db.begin().await?;
        let environment = environments::Entity::find_by_id(environment_id)
            .lock_exclusive()
            .one(&transaction)
            .await?;
        let Some(environment) = environment else {
            return Ok(false);
        };
        if environment.current_deployment_id != Some(deployment_id) {
            return Ok(false);
        }

        let deployment = deployments::Entity::find_by_id(deployment_id)
            .lock_exclusive()
            .one(&transaction)
            .await?;
        if deployment
            .as_ref()
            .is_none_or(|model| model.state == "paused")
        {
            return Ok(false);
        }

        let monitor = status_monitors::Entity::find_by_id(monitor_id)
            .lock_exclusive()
            .one(&transaction)
            .await?;
        if monitor.as_ref().and_then(|model| model.environment_id) != Some(environment_id)
            || monitor.as_ref().is_none_or(|model| {
                !model.is_active || model.updated_at != expected_monitor_updated_at
            })
        {
            return Ok(false);
        }

        check.insert(&transaction).await?;
        transaction.commit().await?;
        Ok(true)
    }

    /// Initialize monitors for all existing environments
    pub async fn initialize_monitors(&self) -> Result<(), StatusPageError> {
        debug!("Initializing monitors for all existing environments");

        // Get all active (non-deleted) environments with their projects
        let environments_with_projects = environments::Entity::find()
            .filter(environments::Column::DeletedAt.is_null())
            .inner_join(projects::Entity)
            .all(self.db.as_ref())
            .await?;

        let monitor_service = super::monitor_service::MonitorService::new(
            self.db.clone(),
            self.config_service.clone(),
        );

        for env in environments_with_projects {
            match monitor_service
                .ensure_monitor_for_environment(env.project_id, env.id, &env.name)
                .await
            {
                Ok(monitor) => {
                    debug!(
                        "Ensured monitor {} for environment {} ({})",
                        monitor.id, env.id, env.name
                    );
                }
                Err(e) => {
                    warn!(
                        "Failed to create monitor for environment {} ({}): {:?}",
                        env.id, env.name, e
                    );
                }
            }
        }

        debug!("Monitor initialization completed");
        Ok(())
    }

    /// Start the periodic health check scheduler with realtime monitor creation handling
    ///
    /// This scheduler:
    /// 1. Initializes monitors for all existing environments at startup
    /// 2. Sweeps every 15 seconds and checks the monitors that are due,
    ///    honouring each monitor's own `check_interval_seconds`
    /// 3. Listens for MonitorCreated events and immediately checks new monitors
    ///
    /// The job_receiver parameter allows the scheduler to react to monitor creation
    /// events in realtime, ensuring new monitors are checked immediately without
    /// waiting for the next scheduled cycle.
    pub async fn start_scheduler(self: Arc<Self>, mut job_receiver: Box<dyn JobReceiver>) {
        debug!("Starting health check scheduler with realtime monitor creation handling");

        // See STARTUP_GRACE_PERIOD: wait out the proxy's boot window before
        // touching any monitor. This also delays `initialize_monitors` below,
        // which prevents the MonitorCreated events it emits for newly-seen
        // environments from triggering an immediate check during the same
        // race window.
        sleep(STARTUP_GRACE_PERIOD).await;

        // Initialize monitors for all environments first
        if let Err(e) = self.initialize_monitors().await {
            error!("Failed to initialize monitors: {:?}", e);
        }

        // Start the periodic check cycle
        let service_for_interval = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SCHEDULER_SWEEP_INTERVAL);
            loop {
                interval.tick().await;
                let service = service_for_interval.clone();
                tokio::spawn(async move {
                    // A failing sweep (database unreachable) repeats every
                    // SCHEDULER_SWEEP_INTERVAL; log it once per outage.
                    match service.run_all_checks().await {
                        Ok(()) => {
                            if let Some(failures) = SWEEP_FAILURES.record_success() {
                                info!(
                                    previous_failures = failures,
                                    "Health check cycles recovered"
                                );
                            }
                        }
                        Err(e) => match SWEEP_FAILURES.record_failure() {
                            FailureLog::Started => error!("Health check cycle failed: {:?}", e),
                            FailureLog::Reminder { consecutive } => error!(
                                consecutive_failures = consecutive,
                                "Health check cycles are still failing: {:?}", e
                            ),
                            FailureLog::Suppressed { consecutive } => debug!(
                                consecutive_failures = consecutive,
                                "Health check cycle failed: {:?}", e
                            ),
                        },
                    }
                });
            }
        });

        // Listen for MonitorCreated events and check new monitors immediately
        loop {
            match job_receiver.recv().await {
                Ok(Job::MonitorCreated(job)) => {
                    info!(
                        "Received MonitorCreated event for monitor {} (environment {}), checking immediately",
                        job.monitor_id, job.environment_id
                    );

                    let service = self.clone();
                    tokio::spawn(async move {
                        // Fetch the monitor and check it immediately
                        match status_monitors::Entity::find_by_id(job.monitor_id)
                            .one(service.db.as_ref())
                            .await
                        {
                            Ok(Some(monitor)) => {
                                // Same reasoning as `check_monitors_for_environment`:
                                // claim the slot before probing so the sweep
                                // does not immediately re-probe this monitor.
                                Self::schedule_next_check(service.db.as_ref(), &monitor).await;
                                if let Err(e) = Self::check_monitor_with(
                                    service.db.clone(),
                                    service.config_service.clone(),
                                    monitor,
                                    service.job_queue.clone(),
                                    service.addresses(),
                                )
                                .await
                                {
                                    error!(
                                        "Failed to check newly created monitor {}: {:?}",
                                        job.monitor_id, e
                                    );
                                } else {
                                    info!(
                                        "Successfully checked newly created monitor {}",
                                        job.monitor_id
                                    );
                                }
                            }
                            Ok(None) => {
                                warn!(
                                    "Monitor {} not found after MonitorCreated event",
                                    job.monitor_id
                                );
                            }
                            Err(e) => {
                                error!("Failed to fetch monitor {}: {:?}", job.monitor_id, e);
                            }
                        }
                    });
                }
                Ok(_) => {
                    // Ignore other job types
                }
                Err(QueueError::ChannelClosed) => {
                    // Retrying a closed channel would fail (and log) every
                    // second forever. The periodic sweep keeps running; only
                    // the immediate check of newly created monitors stops.
                    error!(
                        "Job queue closed; health check scheduler stops listening for MonitorCreated events"
                    );
                    break;
                }
                Err(e) => {
                    // Lagging behind a burst of unrelated jobs: some
                    // MonitorCreated events may have been skipped, and those
                    // monitors are picked up by the next periodic sweep.
                    warn!("Health check scheduler missed queued jobs: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Filter out monitors whose environment has on_demand enabled.
    /// Health checks go through the proxy and reset the idle timer,
    /// which would prevent scale-to-zero from ever triggering.
    fn filter_on_demand_monitors(
        monitors_with_envs: Vec<(status_monitors::Model, Option<environments::Model>)>,
    ) -> Vec<status_monitors::Model> {
        monitors_with_envs
            .into_iter()
            .filter(|(monitor, env)| {
                if let Some(env) = env {
                    let is_on_demand = env
                        .deployment_config
                        .as_ref()
                        .map(|dc| dc.on_demand)
                        .unwrap_or(false);
                    if is_on_demand {
                        debug!(
                            "Skipping monitor {} for on-demand environment {} ({})",
                            monitor.id, env.id, env.name
                        );
                        return false;
                    }
                }
                true
            })
            .map(|(monitor, _)| monitor)
            .collect()
    }

    /// Check a specific environment using its deployment URL
    pub async fn check_environment(
        &self,
        environment_id: i32,
    ) -> Result<(String, Option<i32>), StatusPageError> {
        // Get the environment
        let _environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::NotFound)?;

        // Get the latest deployment
        let deployment = deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::State.eq("completed"))
            .one(self.db.as_ref())
            .await?;

        let Some(deployment) = deployment else {
            return Ok(("no_deployment".to_string(), None));
        };

        // Get the deployment container
        let container = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
            .one(self.db.as_ref())
            .await?;

        let Some(container) = container else {
            return Ok(("no_container".to_string(), None));
        };

        let Some(container_port) = container.container_port else {
            return Ok(("unknown".to_string(), None));
        };
        // Construct the check URL
        let check_url = format!("http://{}:{}/", container.container_name, container_port);

        // Perform the check
        let start_time = std::time::Instant::now();
        let check_result = timeout(
            Duration::from_secs(10),
            self.http_client.get(&check_url).send(),
        )
        .await;

        let response_time_ms = start_time.elapsed().as_millis() as i32;

        match check_result {
            Ok(Ok(response)) => {
                let code = response.status();
                let status = if Self::is_operational_http_status(code) {
                    "operational"
                } else if code.is_server_error() {
                    "major_outage"
                } else {
                    "degraded"
                };
                Ok((status.to_string(), Some(response_time_ms)))
            }
            Ok(Err(_)) => Ok(("major_outage".to_string(), Some(response_time_ms))),
            Err(_) => Ok(("major_outage".to_string(), Some(10000))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gated_answer_is_reported_behind_sign_in() {
        let local = |header| sign_in_note(ProbeOrigin::LocalProxy, 302, header, None);
        assert!(local(Some("sign-in")).is_some_and(|note| note.starts_with("Behind sign-in")));
        assert!(sign_in_note(ProbeOrigin::LocalProxy, 403, Some("access"), None).is_some());
        assert!(local(None).is_none());
    }

    /// A gate that cannot run is an outage, not "behind sign-in", and a
    /// value the gate never sends is not taken for it.
    #[test]
    fn an_unavailable_gate_is_not_reported_behind_sign_in() {
        let local = |header| sign_in_note(ProbeOrigin::LocalProxy, 503, header, None);
        assert!(local(Some(temps_core::gate::GATE_UNAVAILABLE)).is_none());
        assert!(local(Some("anything-else")).is_none());
        assert!(local(Some("")).is_none());
    }

    /// A direct probe reaches whatever answers at the public address, so a
    /// gate header there proves nothing: an app failing with
    /// `x-temps-gate: access` stays an outage.
    #[test]
    fn a_direct_probe_does_not_trust_the_gate_header() {
        let direct = ProbeOrigin::Direct {
            access_team: Some("team.cloudflareaccess.com"),
        };
        for header in ["access", "sign-in"] {
            assert!(sign_in_note(direct, 503, Some(header), None).is_none());
            assert!(sign_in_note(direct, 403, Some(header), None).is_none());
        }
        assert!(!HealthCheckService::is_operational_http_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
    }

    /// Directly, only a redirect to the sign-in of the address's own Access
    /// team is the gate.
    #[test]
    fn a_direct_probe_recognises_its_own_access_sign_in() {
        let own = "https://team.cloudflareaccess.com/cdn-cgi/access/login/app.example.com";
        let other = "https://other.cloudflareaccess.com/cdn-cgi/access/login/app.example.com";
        let direct = ProbeOrigin::Direct {
            access_team: Some("team.cloudflareaccess.com"),
        };
        assert!(sign_in_note(direct, 302, None, Some(own)).is_some());
        assert!(sign_in_note(direct, 303, None, Some(own)).is_some());
        assert!(sign_in_note(direct, 302, None, Some(other)).is_none());
        assert!(sign_in_note(direct, 200, None, Some(own)).is_none());
        assert!(
            sign_in_note(direct, 302, None, Some("http://team.cloudflareaccess.com/")).is_none()
        );
        let unbound = ProbeOrigin::Direct { access_team: None };
        assert!(sign_in_note(unbound, 302, Some("access"), Some(own)).is_none());
    }

    #[test]
    fn down_monitor_is_logged_on_transitions_only() {
        let latch = KeyedFailureLatch::default();

        assert_eq!(report_check_outcome(&latch, 5, "operational", None), None);
        assert_eq!(
            report_check_outcome(&latch, 5, "major_outage", Some("Connection failed")),
            Some(FailureLog::Started)
        );
        // Every further check while the app stays down is quiet.
        for _ in 0..30 {
            let outcome =
                report_check_outcome(&latch, 5, "major_outage", Some("Connection failed"))
                    .expect("a failing check reports an outcome");
            assert!(!outcome.should_log());
        }
        // Recovery re-arms it.
        assert_eq!(report_check_outcome(&latch, 5, "operational", None), None);
        assert!(!latch.is_failing(&5));
        assert_eq!(
            report_check_outcome(&latch, 5, "degraded", Some("HTTP 404")),
            Some(FailureLog::Started)
        );
    }

    #[test]
    fn monitor_check_errors_are_logged_once_per_monitor() {
        let latch = KeyedFailureLatch::default();
        let missing_env = StatusPageError::InvalidRequest("monitor 9 has no environment_id".into());

        assert_eq!(
            report_check_error(&latch, 9, &missing_env),
            FailureLog::Started
        );
        assert!(!report_check_error(&latch, 9, &missing_env).should_log());
        assert_eq!(
            report_check_error(&latch, 10, &StatusPageError::NotFound),
            FailureLog::Started
        );
    }
    use std::io::BufReader;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn spawn_test_tls_server(hostname: &str) -> (SocketAddr, reqwest::Certificate) {
        spawn_test_tls_server_answering(
            hostname,
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
        )
        .await
    }

    /// A TLS server for `hostname` that answers one `GET /ready` with
    /// `response`.
    async fn spawn_test_tls_server_answering(
        hostname: &str,
        response: &'static [u8],
    ) -> (SocketAddr, reqwest::Certificate) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![hostname.to_string()])
                .expect("generate test TLS certificate");
        let cert_pem = cert.pem();
        let key_pem = signing_key.serialize_pem();
        let cert_chain = rustls_pemfile::certs(&mut BufReader::new(cert_pem.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .expect("parse test certificate");
        let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_bytes()))
            .expect("parse test private key")
            .expect("test private key is present");
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .expect("build test TLS server config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test TLS listener");
        let address = listener.local_addr().expect("read TLS listener address");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept TLS probe");
            // A caller that does not trust this test certificate must reject
            // the handshake. That is an expected path in the certificate
            // validation regression below, rather than a server panic.
            let Ok(mut stream) = acceptor.accept(stream).await else {
                return;
            };
            let mut request = [0_u8; 2048];
            let size = stream.read(&mut request).await.expect("read HTTPS probe");
            assert!(
                String::from_utf8_lossy(&request[..size]).starts_with("GET /ready HTTP/1.1\r\n")
            );
            stream
                .write_all(response)
                .await
                .expect("write HTTPS response");
        });
        (
            address,
            reqwest::Certificate::from_pem(cert_pem.as_bytes()).expect("parse reqwest test root"),
        )
    }

    /// The probe host is re-validated at probe time: a hostname or literal
    /// that is (or resolves to) an internal address must never get a direct
    /// client, while a public literal gets a pinned one.
    #[tokio::test]
    async fn public_probe_target_refuses_internal_destinations() {
        for url in [
            "http://127.0.0.1:8080/health",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
            "http://[::1]:8080/",
            "http://localhost:8080/health",
        ] {
            match public_probe_target(url).await {
                Ok(PublicProbeTarget::NonPublic(_)) => {}
                Ok(PublicProbeTarget::Pinned(_)) => panic!("{url} must not get a direct client"),
                Ok(PublicProbeTarget::Unresolved(reason)) => {
                    // Only acceptable for the name, on a host without a
                    // resolver entry for it; still never a direct client.
                    assert!(url.contains("localhost"), "{url}: {reason}");
                }
                Err(error) => panic!("{url}: {error}"),
            }
        }
    }

    #[tokio::test]
    async fn public_probe_target_pins_public_literal() {
        assert!(matches!(
            public_probe_target("https://8.8.8.8/health").await,
            Ok(PublicProbeTarget::Pinned(_))
        ));
        assert!(public_probe_target("not a url").await.is_err());
    }

    #[test]
    fn local_proxy_destination_normalizes_wildcard_addresses() {
        assert_eq!(
            local_proxy_destination("0.0.0.0:8087").expect("valid IPv4 listener"),
            "127.0.0.1:8087".parse().expect("valid expected address")
        );
        assert_eq!(
            local_proxy_destination("[::]:9443").expect("valid IPv6 listener"),
            "[::1]:9443".parse().expect("valid expected address")
        );
        assert_eq!(
            local_proxy_destination("192.0.2.10:8181").expect("valid explicit listener"),
            "192.0.2.10:8181".parse().expect("valid expected address")
        );
    }

    #[test]
    fn only_unconfigured_managed_monitors_route_through_local_proxy() {
        assert!(should_route_probe_locally(true, false));
        assert!(!should_route_probe_locally(true, true));
        assert!(!should_route_probe_locally(false, false));
    }

    #[test]
    fn https_local_probe_requires_a_tls_listener() {
        assert_eq!(
            local_listener_for_url(
                "https://environment.example.test/health",
                "127.0.0.1:80",
                None,
            ),
            None
        );
        assert_eq!(
            local_listener_for_url(
                "https://environment.example.test/health",
                "127.0.0.1:80",
                Some("127.0.0.1:443"),
            ),
            Some("127.0.0.1:443")
        );
        assert_eq!(
            local_listener_for_url(
                "http://environment.example.test/health",
                "127.0.0.1:80",
                None,
            ),
            Some("127.0.0.1:80")
        );
    }

    #[tokio::test]
    async fn local_proxy_client_preserves_logical_host_and_custom_path() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local probe listener");
        let destination = listener.local_addr().expect("read listener address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept local probe");
            let mut request = vec![0_u8; 4096];
            let size = stream.read(&mut request).await.expect("read local probe");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("write local probe response");
            String::from_utf8(request[..size].to_vec()).expect("request is valid HTTP")
        });

        let logical_url = "http://managed-probe.invalid/ready?source=status";
        let client = local_proxy_client(logical_url, &destination.to_string())
            .expect("build pinned local client");
        let response = client
            .get(logical_url)
            .send()
            .await
            .expect("probe reaches local listener");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

        let request = server.await.expect("local probe task completes");
        assert!(
            request.starts_with("GET /ready?source=status HTTP/1.1\r\n"),
            "custom health path must reach the local proxy: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("\r\nhost: managed-probe.invalid\r\n"),
            "logical environment Host must be preserved: {request}"
        );
    }

    #[tokio::test]
    async fn local_proxy_client_does_not_follow_application_redirects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local probe listener");
        let destination = listener.local_addr().expect("read listener address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept local probe");
            let mut request = [0_u8; 1024];
            let _size = stream.read(&mut request).await.expect("read local probe");
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: https://169.254.169.254/latest/meta-data\r\nX-Temps-Proxy-Https-Redirect: 1\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .expect("write redirect response");
        });

        let logical_url = "http://redirecting-probe.invalid/health";
        let client = local_proxy_client(logical_url, &destination.to_string())
            .expect("build pinned local client");
        let response = client
            .get(logical_url)
            .send()
            .await
            .expect("receive original redirect");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert!(
            proxy_https_redirect(logical_url, &response).is_none(),
            "cross-host redirects must never be eligible for the local TLS probe"
        );
        assert_eq!(
            response.url().as_str(),
            logical_url,
            "redirect target must never be requested"
        );
        server.await.expect("local probe task completes");
    }

    #[tokio::test]
    async fn marked_proxy_https_redirect_is_eligible_for_local_tls_probe() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local probe listener");
        let destination = listener.local_addr().expect("read listener address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept local probe");
            let mut request = [0_u8; 1024];
            let _size = stream.read(&mut request).await.expect("read local probe");
            stream
                .write_all(
                    b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://secure-probe.invalid/ready\r\nX-Temps-Proxy-Https-Redirect: 1\r\nX-Temps-Proxy-Probe-Capable: 1\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .expect("write redirect response");
        });

        let logical_url = "http://secure-probe.invalid/ready";
        let response = local_proxy_client(logical_url, &destination.to_string())
            .expect("build pinned local client")
            .get(logical_url)
            .send()
            .await
            .expect("receive HTTPS upgrade redirect");
        assert_eq!(
            proxy_https_redirect(logical_url, &response).as_deref(),
            Some("https://secure-probe.invalid/ready")
        );
        server.await.expect("local probe task completes");
    }

    #[tokio::test]
    async fn unmarked_same_host_application_redirect_remains_operational_without_tls_follow_up() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind application redirect listener");
        let destination = listener.local_addr().expect("read listener address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept application probe");
            let mut request = [0_u8; 1024];
            let _size = stream
                .read(&mut request)
                .await
                .expect("read application probe");
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: https://application-redirect.invalid/login\r\nX-Temps-Proxy-Probe-Capable: 1\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .expect("write application redirect");
        });
        let logical_url = "http://application-redirect.invalid/health";
        let response = local_proxy_client(logical_url, &destination.to_string())
            .expect("build application probe client")
            .get(logical_url)
            .send()
            .await
            .expect("receive application redirect");

        assert!(proxy_https_redirect(logical_url, &response).is_none());
        assert!(proxy_supports_probe_markers(&response));
        assert!(local_https_follow_up_url(logical_url, &response).is_none());
        assert!(HealthCheckService::is_operational_http_status(
            response.status()
        ));
        server.await.expect("application redirect task completes");
    }

    #[tokio::test]
    async fn old_proxy_same_host_https_redirect_uses_safe_tls_follow_up() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind old proxy listener");
        let destination = listener.local_addr().expect("read listener address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept old proxy probe");
            let mut request = [0_u8; 1024];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read old proxy probe");
            stream
                .write_all(
                    b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://old-proxy.invalid/health\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .expect("write old proxy redirect");
        });
        let logical_url = "http://old-proxy.invalid/health";
        let response = local_proxy_client(logical_url, &destination.to_string())
            .expect("build old proxy client")
            .get(logical_url)
            .send()
            .await
            .expect("receive old proxy redirect");

        assert!(response.status().is_redirection());
        assert!(!proxy_supports_probe_markers(&response));
        assert_eq!(
            local_https_follow_up_url(logical_url, &response),
            Some(("https://old-proxy.invalid/health".to_string(), true))
        );
        assert_eq!(https_follow_up_failure_status(true), "degraded");
        assert_eq!(https_follow_up_failure_status(false), "major_outage");
        server.await.expect("old proxy task completes");
    }

    #[tokio::test]
    async fn local_https_follow_up_pins_host_validates_tls_and_reaches_application_path() {
        let hostname = "secure-probe.invalid";
        let (destination, root) = spawn_test_tls_server(hostname).await;
        let logical_url = format!("https://{hostname}/ready");
        let client = local_proxy_client_with_roots(&logical_url, &destination.to_string(), &[root])
            .expect("build pinned TLS client with test root");

        let (response, attempts) =
            send_with_timeout_retries(&client, &logical_url, Duration::from_secs(1))
                .await
                .expect("HTTPS follow-up succeeds");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert_eq!(attempts, 1);
        assert_eq!(response.url().host_str(), Some(hostname));
    }

    /// An address that forces HTTPS shows its gate only on the follow-up:
    /// a Cloudflare Access refusal at the origin, marked by the proxy, comes
    /// back over the pinned TLS connection and counts as behind sign-in.
    #[tokio::test]
    async fn gate_answer_on_the_https_follow_up_is_behind_sign_in() {
        let hostname = "gated-probe.invalid";
        let (destination, root) = spawn_test_tls_server_answering(
            hostname,
            b"HTTP/1.1 403 Forbidden\r\nX-Temps-Gate: access\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        let logical_url = format!("https://{hostname}/ready");
        let client = local_proxy_client_with_roots(&logical_url, &destination.to_string(), &[root])
            .expect("build pinned TLS client with test root");

        let (response, _) =
            send_with_timeout_retries(&client, &logical_url, Duration::from_secs(1))
                .await
                .expect("HTTPS follow-up succeeds");
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        assert!(local_sign_in_note(&response).is_some());
    }

    #[tokio::test]
    async fn managed_redirect_tls_certificate_failure_records_outage() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HTTP proxy test listener");
        let http_address = http_listener
            .local_addr()
            .expect("read HTTP listener address");
        let initial_config = temps_config::ServerConfig::new(
            http_address.to_string(),
            test_db.database_url.clone(),
            None,
            None,
        )
        .expect("build initial server config");
        let initial_config_service =
            temps_config::ConfigService::new(Arc::new(initial_config), db.clone());
        let subdomain = "tls-follow-up-coverage";
        let (public_url, is_external) = initial_config_service
            .get_deployment_url_by_slug_with_source(subdomain)
            .await
            .expect("resolve generated health URL");
        assert!(!is_external);
        let hostname = reqwest::Url::parse(&public_url)
            .expect("parse generated URL")
            .host_str()
            .expect("generated URL has host")
            .to_string();
        let (tls_address, _untrusted_root) = spawn_test_tls_server(&hostname).await;
        let config = temps_config::ServerConfig::new(
            http_address.to_string(),
            test_db.database_url.clone(),
            Some(tls_address.to_string()),
            None,
        )
        .expect("build split proxy config");
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(config),
            db.clone(),
        ));

        let project = temps_entities::projects::ActiveModel {
            name: Set("TLS Follow-up Test".to_string()),
            repo_name: Set("tls-follow-up".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("tls-follow-up".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert test project");
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(subdomain.to_string()),
            host: Set(hostname.clone()),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert test environment");
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("tls-follow-up-deployment".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert test deployment");
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(deployment.id));
        let environment = active_environment
            .update(db.as_ref())
            .await
            .expect("select test deployment");
        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("managed TLS health".to_string()),
            monitor_type: Set("web".to_string()),
            check_path: Set(Some("/ready".to_string())),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            is_managed: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert managed monitor");

        let redirect_url = format!("https://{hostname}/ready");
        let http_server = tokio::spawn(async move {
            let (mut stream, _) = http_listener.accept().await.expect("accept HTTP probe");
            let mut request = [0_u8; 2048];
            let size = stream.read(&mut request).await.expect("read HTTP probe");
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("GET /ready HTTP/1.1"));
            let response = format!(
                "HTTP/1.1 301 Moved Permanently\r\nLocation: {redirect_url}\r\nX-Temps-Proxy-Https-Redirect: 1\r\nX-Temps-Proxy-Probe-Capable: 1\r\nContent-Length: 0\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("send proxy-owned redirect");
        });
        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);
        HealthCheckService::check_monitor(db.clone(), config_service, monitor.clone(), job_queue)
            .await
            .expect("record failed TLS verification");
        http_server.await.expect("proxy redirect was served");

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .expect("load recorded checks");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, "major_outage");
        assert!(checks[0].response_time_ms.is_some());
        assert!(checks[0]
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("Local HTTPS proxy probe failed")));
    }

    #[tokio::test]
    async fn https_follow_up_retries_all_bounded_timeouts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind timeout listener");
        let destination = listener.local_addr().expect("read timeout address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_server = Arc::clone(&accepted);
        let server = tokio::spawn(async move {
            let mut held_streams = Vec::new();
            for _ in 0..=MAX_RETRIES {
                let (stream, _) = listener.accept().await.expect("accept timed-out probe");
                accepted_by_server.fetch_add(1, Ordering::SeqCst);
                held_streams.push(stream);
            }
            held_streams
        });
        let logical_url = "http://timeout-probe.invalid/ready";
        let client = local_proxy_client(logical_url, &destination.to_string())
            .expect("build pinned timeout client");

        let error = send_with_timeout_retries(&client, logical_url, Duration::from_millis(20))
            .await
            .expect_err("all attempts should time out");
        assert!(matches!(
            error,
            ProbeSendError::Timeout { attempts } if attempts == MAX_RETRIES + 1
        ));
        server.abort();
        assert_eq!(accepted.load(Ordering::SeqCst), (MAX_RETRIES + 1) as usize);
    }

    #[test]
    fn explicit_root_path_probes_deployment_root_for_health_monitor() {
        assert_eq!(
            probe_url("https://app.example.test/", "health", Some("/"))
                .expect("root is a valid check path"),
            "https://app.example.test"
        );
        assert_eq!(
            probe_url("https://app.example.test/", "health", None)
                .expect("default health path should resolve"),
            "https://app.example.test/health"
        );
    }
    use temps_entities::deployment_config::DeploymentConfig;
    use temps_entities::upstream_config::UpstreamList;

    fn make_monitor(id: i32, env_id: Option<i32>) -> status_monitors::Model {
        status_monitors::Model {
            id,
            project_id: 1,
            environment_id: env_id,
            name: format!("monitor-{}", id),
            monitor_type: "web".to_string(),
            check_path: None,
            check_path_revision: 0,
            check_interval_seconds: 60,
            is_active: true,
            is_managed: false,
            next_check_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_env(id: i32, on_demand: bool) -> environments::Model {
        let deployment_config = if on_demand {
            Some(DeploymentConfig {
                on_demand: true,
                idle_timeout_seconds: 60,
                ..Default::default()
            })
        } else {
            Some(DeploymentConfig::default())
        };

        environments::Model {
            id,
            name: format!("env-{}", id),
            slug: format!("env-{}", id),
            subdomain: format!("proj-env-{}", id),
            last_deployment: None,
            host: String::new(),
            upstreams: UpstreamList::default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            project_id: 1,
            current_deployment_id: Some(1),
            branch: None,
            deleted_at: None,
            deployment_config,
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            force_https: None,
            last_activity_at: None,
            ingress_mode: Default::default(),
            workload_mode: Default::default(),
        }
    }

    #[test]
    fn interval_below_floor_is_clamped() {
        assert_eq!(
            HealthCheckService::effective_check_interval_secs(5),
            i64::from(super::super::types::MIN_CHECK_INTERVAL_SECS)
        );
        assert_eq!(
            HealthCheckService::effective_check_interval_secs(29),
            i64::from(super::super::types::MIN_CHECK_INTERVAL_SECS)
        );
        assert_eq!(
            HealthCheckService::effective_check_interval_secs(-120),
            i64::from(super::super::types::DEFAULT_CHECK_INTERVAL_SECS)
        );
    }

    #[test]
    fn unset_interval_falls_back_to_default() {
        assert_eq!(
            HealthCheckService::effective_check_interval_secs(0),
            i64::from(super::super::types::DEFAULT_CHECK_INTERVAL_SECS)
        );
    }

    #[test]
    fn configured_interval_above_floor_is_preserved() {
        assert_eq!(HealthCheckService::effective_check_interval_secs(30), 30);
        assert_eq!(HealthCheckService::effective_check_interval_secs(600), 600);
    }

    /// The whole point of the due column: the sweep must ask the database for
    /// the monitors whose turn it is, not for every active monitor. A
    /// regression here is invisible at runtime (checks still happen, just far
    /// too often), so the predicate is asserted directly.
    #[test]
    fn due_query_selects_only_active_monitors_that_are_due() {
        use sea_orm::{DbBackend, QueryTrait};

        let sql = HealthCheckService::due_monitors_query(Utc::now())
            .build(DbBackend::Postgres)
            .to_string();

        assert!(
            sql.contains("\"is_active\" = TRUE"),
            "sweep must stay limited to active monitors: {sql}"
        );
        assert!(
            sql.contains("\"next_check_at\" IS NULL"),
            "never-scheduled monitors must be treated as due: {sql}"
        );
        assert!(
            sql.contains("\"next_check_at\" <="),
            "the sweep must filter on the due time in SQL: {sql}"
        );
    }

    #[test]
    fn test_filter_skips_on_demand_monitors() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, true))),
            (make_monitor(2, Some(20)), Some(make_env(20, false))),
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 2);
    }

    #[test]
    fn test_filter_keeps_monitors_without_environment() {
        let input = vec![(make_monitor(1, None), None)];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 1);
    }

    #[test]
    fn test_filter_keeps_normal_monitors() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, false))),
            (make_monitor(2, Some(20)), Some(make_env(20, false))),
            (make_monitor(3, Some(30)), Some(make_env(30, false))),
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_filter_skips_all_on_demand() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, true))),
            (make_monitor(2, Some(20)), Some(make_env(20, true))),
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_keeps_monitor_with_no_deployment_config() {
        let mut env = make_env(10, false);
        env.deployment_config = None;

        let input = vec![(make_monitor(1, Some(10)), Some(env))];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_mixed_on_demand_and_normal() {
        let input = vec![
            (make_monitor(1, Some(10)), Some(make_env(10, true))), // on-demand -> skip
            (make_monitor(2, Some(20)), Some(make_env(20, false))), // normal -> keep
            (make_monitor(3, None), None),                         // no env -> keep
            (make_monitor(4, Some(40)), Some(make_env(40, true))), // on-demand -> skip
            (make_monitor(5, Some(50)), Some(make_env(50, false))), // normal -> keep
        ];

        let result = HealthCheckService::filter_on_demand_monitors(input);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, 2);
        assert_eq!(result[1].id, 3);
        assert_eq!(result[2].id, 5);
    }

    #[test]
    fn test_operational_http_status_matches_deployment_readiness() {
        for status in [200, 204, 301, 302, 307, 308] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(
                HealthCheckService::is_operational_http_status(status),
                "HTTP {status} should be operational"
            );
        }

        for status in [400, 401, 403, 404, 405, 429, 500, 502, 503] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(
                !HealthCheckService::is_operational_http_status(status),
                "HTTP {status} should not be operational"
            );
        }
    }

    // ── check_monitor: paused-deployment skip ──────────────────────────
    //
    // Regression coverage for the gap Greptile flagged on PR #835: the
    // paused check used to live only in `run_all_checks`'s pre-filter, so
    // the immediate check fired by the `MonitorCreated` job (which calls
    // `check_monitor` directly, bypassing that pre-filter) still reported a
    // freshly-paused deployment as down. The check now lives inside
    // `check_monitor` itself — the single place every caller goes through —
    // and reads deployment state live rather than from a value the caller
    // captured earlier, so it can't be stale relative to
    // `pause_deployment`, which persists `state = "paused"` before it stops
    // any containers.

    struct NeverJobQueue;
    #[async_trait::async_trait]
    impl temps_core::JobQueue for NeverJobQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            struct NeverReceiver;
            #[async_trait::async_trait]
            impl temps_core::JobReceiver for NeverReceiver {
                async fn recv(&mut self) -> Result<temps_core::Job, temps_core::QueueError> {
                    std::future::pending().await
                }
            }
            Box::new(NeverReceiver)
        }
    }

    fn test_config_service(
        db: &Arc<DatabaseConnection>,
        database_url: &str,
    ) -> Arc<temps_config::ConfigService> {
        let config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            database_url.to_string(),
            None,
            None,
        )
        .expect("failed to build test ServerConfig");
        Arc::new(temps_config::ConfigService::new(
            Arc::new(config),
            db.clone(),
        ))
    }

    fn health_probe_deployment() -> deployments::Model {
        serde_json::from_value(serde_json::json!({
            "id": 1, "project_id": 1, "environment_id": 1,
            "slug": "worker", "state": "completed", "docker_socket_mounted": false,
            "created_at": Utc::now(), "updated_at": Utc::now()
        }))
        .unwrap()
    }

    fn health_probe_counts(
        containers: i64,
        ports: i64,
    ) -> std::collections::BTreeMap<String, sea_orm::Value> {
        std::collections::BTreeMap::from([
            ("container_count".into(), containers.into()),
            ("port_count".into(), ports.into()),
        ])
    }

    #[tokio::test]
    async fn health_probe_portless_monitor_requests_retirement_without_checking_http() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        // Every entry path (periodic, MonitorCreated and DeploymentSucceeded)
        // uses check_monitor. Supplying only these reads also makes any attempt
        // to resolve an HTTP URL or write a status check fail the test.
        for managed in [true, false] {
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([vec![make_env(1, false)]])
                    .append_query_results([vec![health_probe_deployment()]])
                    .append_query_results([vec![health_probe_counts(2, 0)]])
                    .append_query_results([Vec::<status_checks::Model>::new()])
                    .into_connection(),
            );
            let config = test_config_service(&db, "postgres://localhost/test");
            let mut monitor = make_monitor(1, Some(1));
            monitor.is_managed = managed;
            let queue: Arc<dyn JobQueue> = Arc::new(RejectHealthProbeJobs);
            HealthCheckService::check_monitor(db.clone(), config, monitor, queue)
                .await
                .unwrap();
            let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
            assert_eq!(log.len(), 4);
            let log = format!("{log:?}");
            assert!(!log.contains("INSERT"));
            assert!(
                log.contains("deleted_at"),
                "retired containers must not decide portlessness"
            );
        }
    }

    #[tokio::test]
    async fn health_probe_retired_http_monitor_does_not_repeat_retirement_jobs() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        struct NoJobs;
        #[async_trait::async_trait]
        impl JobQueue for NoJobs {
            async fn send(&self, _: Job) -> Result<(), QueueError> {
                panic!("completed retirement must not be requeued");
            }
            fn subscribe(&self) -> Box<dyn JobReceiver> {
                panic!("not used");
            }
        }
        let now = Utc::now();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![make_env(1, false)]])
                .append_query_results([vec![health_probe_deployment()]])
                .append_query_results([vec![health_probe_counts(1, 0)]])
                .append_query_results([vec![status_checks::Model {
                    id: 1,
                    monitor_id: 1,
                    status: "unknown".into(),
                    checked_at: now,
                    created_at: now,
                    response_time_ms: None,
                    error_message: Some(
                        "HTTP monitoring does not apply to a background worker".into(),
                    ),
                }]])
                .into_connection(),
        );
        HealthCheckService::check_monitor(
            db.clone(),
            test_config_service(&db, "postgres://localhost/test"),
            make_monitor(1, Some(1)),
            Arc::new(NoJobs),
        )
        .await
        .unwrap();
    }

    /// An environment whose reachability is settled.
    struct FixedAddress(EnvironmentAddress);

    #[async_trait::async_trait]
    impl EnvironmentAddresses for FixedAddress {
        async fn address(
            &self,
            _project_id: i32,
            _environment_id: i32,
        ) -> Result<EnvironmentAddress, temps_core::environment_address::EnvironmentAddressError>
        {
            Ok(self.0.clone())
        }
        fn cached(&self, _environment_id: i32) -> Option<EnvironmentAddress> {
            Some(self.0.clone())
        }
        async fn deployment_completed(
            &self,
            _project_id: i32,
            _environment_id: i32,
        ) -> Option<temps_core::environment_address::ReachabilityWarning> {
            None
        }
    }

    fn settled(
        primary: Option<(&str, AddressKind)>,
        no_address: Option<&str>,
    ) -> EnvironmentAddress {
        EnvironmentAddress {
            environment_id: 1,
            primary: primary.map(|(address, kind)| {
                temps_core::environment_address::PrimaryAddress {
                    address: address.into(),
                    kind,
                    way_in: None,
                }
            }),
            no_address_reason: no_address.map(str::to_string),
            warning: None,
        }
    }

    #[test]
    fn the_probe_follows_the_primary_address() {
        // A custom domain or tunnel host is dialled on https.
        assert_eq!(
            primary_probe_url(Some(&settled(
                Some(("app.example.com", AddressKind::CustomDomain)),
                None
            )))
            .as_deref(),
            Some("https://app.example.com")
        );
        assert_eq!(
            primary_probe_url(Some(&settled(
                Some(("app-production.example.org", AddressKind::TunnelHost)),
                None
            )))
            .as_deref(),
            Some("https://app-production.example.org")
        );
        // The generated host and private addresses keep today's URL and
        // routing, as does no answer at all.
        for kind in [AddressKind::Generated, AddressKind::Private] {
            assert_eq!(
                primary_probe_url(Some(&settled(Some(("x.example.dev", kind)), None))),
                None
            );
        }
        assert_eq!(primary_probe_url(None), None);
        assert!(report_check_outcome(
            &KeyedFailureLatch::default(),
            1,
            NO_ADDRESS_STATUS,
            Some(NO_ADDRESS_MESSAGE)
        )
        .is_none());
    }

    struct ExpectStatus(&'static str);
    #[async_trait::async_trait]
    impl JobQueue for ExpectStatus {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            match job {
                Job::StatusCheckCompleted(job) => assert_eq!(job.status, self.0, "{job:?}"),
                _ => panic!("only the check is emitted"),
            }
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("not used")
        }
    }

    fn settings_row(settings: temps_core::AppSettings) -> temps_entities::settings::Model {
        let now = Utc::now();
        temps_entities::settings::Model {
            id: 1,
            data: settings.to_json(),
            created_at: now,
            updated_at: now,
        }
    }

    /// The rows `persist_check_if_current` reads and writes for `status`.
    fn persisted_check(
        db: sea_orm::MockDatabase,
        monitor: &status_monitors::Model,
        status: &str,
    ) -> sea_orm::MockDatabase {
        let now = Utc::now();
        db.append_query_results([vec![monitor.clone()]])
            .append_query_results([vec![make_env(1, false)]])
            .append_query_results([vec![health_probe_deployment()]])
            .append_query_results([vec![monitor.clone()]])
            .append_query_results([vec![status_checks::Model {
                id: 1,
                monitor_id: monitor.id,
                status: status.into(),
                checked_at: now,
                created_at: now,
                response_time_ms: None,
                error_message: None,
            }]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
    }

    #[tokio::test]
    async fn a_managed_monitor_without_a_public_address_probes_through_the_local_proxy() {
        probe_on_a_local_install(true).await;
    }

    #[tokio::test]
    async fn a_monitor_created_by_hand_without_a_public_address_probes_through_the_local_proxy() {
        // Not managed, so the probe dials the public name first. It resolves
        // to this computer, which is not public, so the probe falls back to
        // the local proxy as before ADR 068 -- never to `no_address`.
        if tokio::net::lookup_host("proj-env-1.localho.st:80")
            .await
            .is_err()
        {
            println!("proj-env-1.localho.st does not resolve here, skipping");
            return;
        }
        probe_on_a_local_install(false).await;
    }

    /// A plain local install: the preview domain is `localho.st`, so nobody
    /// else can reach the app, but Temps' own proxy serves it. The monitor
    /// probes there and reports the app's health, as it did before ADR 068.
    async fn probe_on_a_local_install(is_managed: bool) {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local proxy listener");
        let proxy_address = listener.local_addr().expect("read proxy address");
        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept the probe");
            let mut request = [0_u8; 2048];
            let size = stream.read(&mut request).await.expect("read the probe");
            let request = String::from_utf8_lossy(&request[..size]).to_string();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("answer the probe");
            request
        });
        let mut monitor = make_monitor(1, Some(1));
        monitor.is_managed = is_managed;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![make_env(1, false)]])
            .append_query_results([vec![health_probe_deployment()]])
            .append_query_results([vec![health_probe_counts(1, 1)]])
            .append_query_results([vec![settings_row(temps_core::AppSettings {
                preview_domain: "localho.st".into(),
                external_url: None,
                ..temps_core::AppSettings::default()
            })]]);
        let db = Arc::new(persisted_check(db, &monitor, "operational").into_connection());
        let config = temps_config::ServerConfig::new(
            proxy_address.to_string(),
            "postgres://localhost/test".to_string(),
            None,
            None,
        )
        .expect("build test ServerConfig");
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(config),
            db.clone(),
        ));
        let addresses: Arc<dyn EnvironmentAddresses> = Arc::new(FixedAddress(settled(
            None,
            Some("proj-env-1.localho.st only resolves to this computer"),
        )));
        HealthCheckService::check_monitor_with(
            db.clone(),
            config_service,
            monitor,
            Arc::new(ExpectStatus("operational")),
            Some(addresses),
        )
        .await
        .expect("the check is recorded");
        let request = proxy.await.expect("the local proxy was probed");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("host: proj-env-1.localho.st"),
            "{request}"
        );
        let log = format!(
            "{:?}",
            Arc::try_unwrap(db)
                .expect("one owner")
                .into_transaction_log()
        );
        assert!(log.contains("operational"), "{log}");
        assert!(!log.contains(NO_ADDRESS_STATUS), "{log}");
    }

    #[tokio::test]
    async fn an_unresolved_name_with_nothing_set_up_records_no_address() {
        // The probe goes where it always went: out over the public internet
        // for a monitor created by hand, or for a managed one behind an
        // external URL. The name does not resolve (`.invalid` never does,
        // RFC 6761) and the environment was never given an address, so the
        // failure is `no_address` rather than an outage.
        use sea_orm::{DatabaseBackend, MockDatabase};
        for (is_managed, external_url) in [
            (false, None),
            (true, Some("https://temps.example.invalid".to_string())),
        ] {
            let mut monitor = make_monitor(1, Some(1));
            monitor.is_managed = is_managed;
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![make_env(1, false)]])
                .append_query_results([vec![health_probe_deployment()]])
                .append_query_results([vec![health_probe_counts(1, 1)]])
                .append_query_results([vec![settings_row(temps_core::AppSettings {
                    preview_domain: "apps.example.invalid".into(),
                    external_url,
                    ..temps_core::AppSettings::default()
                })]]);
            let db = Arc::new(persisted_check(db, &monitor, NO_ADDRESS_STATUS).into_connection());
            let addresses: Arc<dyn EnvironmentAddresses> = Arc::new(FixedAddress(settled(
                None,
                Some("proj-env-1.apps.example.invalid does not resolve"),
            )));
            HealthCheckService::check_monitor_with(
                db.clone(),
                test_config_service(&db, "postgres://localhost/test"),
                monitor,
                Arc::new(ExpectStatus(NO_ADDRESS_STATUS)),
                Some(addresses),
            )
            .await
            .expect("no_address is recorded");
            let log = format!(
                "{:?}",
                Arc::try_unwrap(db)
                    .expect("one owner")
                    .into_transaction_log()
            );
            assert!(log.contains("no_address"), "managed={is_managed}: {log}");
        }
    }

    #[tokio::test]
    async fn health_probe_of_a_broken_way_in_records_an_outage_not_no_address() {
        // ADR 068 D4: the primary was a tunnel host and the tunnel is now in
        // error. Reachability gives no primary and no `no_address` reason --
        // a way in was set up and broke -- so the monitor probes the
        // generated URL as before and records the outage.
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
        struct ExpectOutage;
        #[async_trait::async_trait]
        impl JobQueue for ExpectOutage {
            async fn send(&self, job: Job) -> Result<(), QueueError> {
                match job {
                    Job::StatusCheckCompleted(job) => {
                        assert_eq!(job.status, "major_outage");
                        assert_ne!(job.error_message.as_deref(), Some(NO_ADDRESS_MESSAGE));
                    }
                    _ => panic!("only the outage check is emitted"),
                }
                Ok(())
            }
            fn subscribe(&self) -> Box<dyn JobReceiver> {
                panic!("not used")
            }
        }
        let monitor = make_monitor(1, Some(1));
        let now = Utc::now();
        // `.invalid` never resolves (RFC 6761), so the probe fails without
        // reaching any network.
        let settings = temps_entities::settings::Model {
            id: 1,
            data: temps_core::AppSettings {
                preview_domain: "apps.example.invalid".into(),
                ..temps_core::AppSettings::default()
            }
            .to_json(),
            created_at: now,
            updated_at: now,
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![make_env(1, false)]])
                .append_query_results([vec![health_probe_deployment()]])
                .append_query_results([vec![health_probe_counts(1, 1)]])
                .append_query_results([vec![settings]])
                // persist_check_if_current
                .append_query_results([vec![monitor.clone()]])
                .append_query_results([vec![make_env(1, false)]])
                .append_query_results([vec![health_probe_deployment()]])
                .append_query_results([vec![monitor.clone()]])
                .append_query_results([vec![status_checks::Model {
                    id: 1,
                    monitor_id: 1,
                    status: "major_outage".into(),
                    checked_at: now,
                    created_at: now,
                    response_time_ms: None,
                    error_message: Some("DNS resolution failed".into()),
                }]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let addresses: Arc<dyn EnvironmentAddresses> = Arc::new(FixedAddress(settled(None, None)));
        HealthCheckService::check_monitor_with(
            db.clone(),
            test_config_service(&db, "postgres://localhost/test"),
            monitor,
            Arc::new(ExpectOutage),
            Some(addresses),
        )
        .await
        .expect("the outage is recorded");
        let log = format!(
            "{:?}",
            Arc::try_unwrap(db)
                .expect("one owner")
                .into_transaction_log()
        );
        assert!(log.contains("major_outage"), "{log}");
        assert!(!log.contains(NO_ADDRESS_STATUS), "{log}");
    }

    struct RejectHealthProbeJobs;
    #[async_trait::async_trait]
    impl JobQueue for RejectHealthProbeJobs {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            match _job {
                Job::StatusCheckCompleted(job) => assert_eq!(job.status, "not_applicable"),
                _ => panic!("portless HTTP monitoring must only retire its old HTTP state"),
            }
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("probe test does not subscribe")
        }
    }

    #[tokio::test]
    async fn health_probe_http_monitor_only_skips_deployments_with_no_declared_ports() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        for (containers, ports, portless) in
            [(2, 0, true), (2, 1, false), (2, 2, false), (0, 0, false)]
        {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![health_probe_counts(containers, ports)]])
                .into_connection();
            assert_eq!(
                HealthCheckService::is_portless_deployment(&db, 1)
                    .await
                    .unwrap(),
                portless
            );
        }
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("probe metadata unavailable".into())])
            .into_connection();
        assert!(HealthCheckService::is_portless_deployment(&db, 1)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_environment_recheck_selects_monitor_and_skips_paused_deployment() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();

        let project = temps_entities::projects::ActiveModel {
            name: Set("Paused Skip Test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("paused-skip-test".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("paused-skip-test-production".to_string()),
            host: Set("paused-skip-test-production.test.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("paused-skip-test-1".to_string()),
            state: Set("paused".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let mut active_environment: environments::ActiveModel = environment.clone().into();
        active_environment.current_deployment_id = Set(Some(deployment.id));
        let environment = active_environment.update(db.as_ref()).await.unwrap();

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("production health".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let config_service = test_config_service(&db, &test_db.database_url);
        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);
        let health_check_service = HealthCheckService::new(db.clone(), config_service, job_queue)
            .expect("test HTTP client should build");

        // If the paused check were skipped or stale, this would try to hit
        // `paused-skip-test-production.test.local`, which doesn't resolve —
        // the call would still return `Ok(())` (check_monitor treats a
        // failed request as a recorded outage, not an error) but it would
        // insert a `status_checks` row. Assert none was written instead of
        // asserting on the return value, so the test fails loudly if the
        // guard regresses instead of passing for the wrong reason.
        let checked = health_check_service
            .check_monitors_for_environment(project.id, environment.id)
            .await
            .expect("post-deploy environment recheck should succeed");
        assert_eq!(checked, 1, "the environment's active monitor is selected");

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "check_monitor must not perform an HTTP check against a paused deployment"
        );
    }

    /// Regression test for the mid-check race Greptile flagged: `check_monitor`'s
    /// own paused guard only runs once, before the HTTP request — but that
    /// request (with up to 3 retries) can take several seconds, long enough
    /// for a pause to land in between. `record_check` must re-check live,
    /// right before persisting any outcome, regardless of what the caller
    /// observed earlier. Exercise `record_check` directly (rather than the
    /// full `check_monitor`, whose early guard would trivially skip a
    /// deployment that's already paused at entry) so this only passes if the
    /// *late* guard inside `record_check` itself is the one doing the work.
    #[tokio::test]
    async fn test_record_check_skips_paused_deployment_mid_check() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();

        let project = temps_entities::projects::ActiveModel {
            name: Set("Mid-Check Pause Test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("mid-check-pause-test".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("mid-check-pause-test-production".to_string()),
            host: Set("mid-check-pause-test-production.test.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        // Deployment starts NOT paused — modeling "check_monitor's early
        // guard saw a running deployment and proceeded to the HTTP request".
        // It's paused (below) only after that point, simulating the pause
        // landing while the check is in flight.
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("mid-check-pause-test-1".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let mut active_environment: environments::ActiveModel = environment.clone().into();
        active_environment.current_deployment_id = Set(Some(deployment.id));
        active_environment.update(db.as_ref()).await.unwrap();

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("production health".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        // Pause lands mid-check, after the (simulated) early guard already
        // passed.
        let mut active_deployment: deployments::ActiveModel = deployment.clone().into();
        active_deployment.state = Set("paused".to_string());
        active_deployment.update(db.as_ref()).await.unwrap();

        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);

        let result = HealthCheckService::record_check(
            &db,
            MonitorProbeSnapshot {
                monitor_id: monitor.id,
                deployment_id: deployment.id,
                monitor_updated_at: monitor.updated_at,
            },
            "major_outage".to_string(),
            Some(5000),
            Some("Connection failed".to_string()),
            &job_queue,
        )
        .await;
        assert!(result.is_ok());

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "record_check must not persist a check result for a deployment paused mid-check, \
             even when the caller's own paused guard already passed"
        );
    }

    #[tokio::test]
    async fn test_record_check_discards_result_from_replaced_deployment() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let project = temps_entities::projects::ActiveModel {
            name: Set("Deployment Replacement Test".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("deployment-replacement-test".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("deployment-replacement-test-production".to_string()),
            host: Set("deployment-replacement-test-production.test.local".to_string()),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let old_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("replacement-old".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let current_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("replacement-current".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let environment_id = environment.id;
        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(current_deployment.id));
        active_environment.update(db.as_ref()).await.unwrap();
        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment_id)),
            name: Set("production health".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);

        HealthCheckService::record_check(
            &db,
            MonitorProbeSnapshot {
                monitor_id: monitor.id,
                deployment_id: old_deployment.id,
                monitor_updated_at: monitor.updated_at,
            },
            "major_outage".to_string(),
            Some(5000),
            Some("stale failure".to_string()),
            &job_queue,
        )
        .await
        .unwrap();

        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "a result started for an older deployment must not become current status"
        );

        let probed_monitor_updated_at = monitor.updated_at;
        let mut changed_monitor: status_monitors::ActiveModel = monitor.clone().into();
        changed_monitor.check_path = Set(Some("/ready".to_string()));
        changed_monitor.update(db.as_ref()).await.unwrap();
        HealthCheckService::record_check(
            &db,
            MonitorProbeSnapshot {
                monitor_id: monitor.id,
                deployment_id: current_deployment.id,
                monitor_updated_at: probed_monitor_updated_at,
            },
            "major_outage".to_string(),
            Some(5000),
            Some("result from the old path".to_string()),
            &job_queue,
        )
        .await
        .unwrap();
        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "a result from a monitor path changed mid-probe must be discarded"
        );
    }

    /// Helper: minimal project + environment for the claim tests below.
    async fn seed_project_and_environment(
        db: &Arc<DatabaseConnection>,
        slug: &str,
        on_demand: bool,
    ) -> (temps_entities::projects::Model, environments::Model) {
        let project = temps_entities::projects::ActiveModel {
            name: Set(slug.to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set(slug.to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let deployment_config = if on_demand {
            Some(temps_entities::deployment_config::DeploymentConfig {
                on_demand: true,
                idle_timeout_seconds: 60,
                ..Default::default()
            })
        } else {
            Some(temps_entities::deployment_config::DeploymentConfig::default())
        };

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(format!("{slug}-production")),
            host: Set(format!("{slug}-production.test.local")),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            deployment_config: Set(deployment_config),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        (project, environment)
    }

    /// The stamp itself: claiming a monitor pushes `next_check_at` out by the
    /// monitor's **own** interval, so a 600s monitor drops out of the due
    /// query for ten minutes instead of being re-selected by the next sweep.
    #[tokio::test]
    async fn claiming_a_monitor_stamps_its_own_interval_and_clears_the_due_query() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) =
            seed_project_and_environment(&db, "claim-stamp-test", false).await;

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("slow monitor".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(600),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        assert!(
            monitor.next_check_at.is_none(),
            "a fresh monitor starts unscheduled and therefore due"
        );

        let before = Utc::now();
        assert!(
            HealthCheckService::claim_due_monitor(db.as_ref(), &monitor).await,
            "an unclaimed due monitor must be claimable"
        );

        let reloaded = status_monitors::Entity::find_by_id(monitor.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("monitor still exists");
        let next = reloaded
            .next_check_at
            .expect("claiming must stamp a due time");
        let delta = (next - before).num_seconds();
        assert!(
            (595..=610).contains(&delta),
            "600s monitor must be pushed out by its own interval, got {delta}s"
        );

        assert!(
            reloaded.updated_at == monitor.updated_at,
            "claiming must not bump updated_at — it is the OCC token \
             persist_check_if_current compares against"
        );

        // And it is genuinely out of the sweep's result set now.
        let due_ids: Vec<i32> = HealthCheckService::due_monitors_query(Utc::now())
            .all(db.as_ref())
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert!(
            !due_ids.contains(&monitor.id),
            "a claimed 600s monitor must not be due again on the next sweep"
        );
    }

    /// Two overlapping sweeps observe the same due row (the sweep spawns each
    /// cycle without awaiting the previous one, so this is the normal case
    /// under load, not a pathology). Exactly one may probe it.
    #[tokio::test]
    async fn overlapping_sweeps_claim_a_due_monitor_exactly_once() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) =
            seed_project_and_environment(&db, "claim-overlap-test", false).await;

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("contended monitor".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        // Both sweeps selected the row before either wrote, so both hold the
        // same observed snapshot — the exact race the CAS has to settle.
        let sweep_one = monitor.clone();
        let sweep_two = monitor.clone();

        let first = HealthCheckService::claim_due_monitor(db.as_ref(), &sweep_one).await;
        let second = HealthCheckService::claim_due_monitor(db.as_ref(), &sweep_two).await;

        assert!(first, "the first sweep to reach the row wins the claim");
        assert!(
            !second,
            "the second sweep must see rows_affected == 0 and skip the probe \
             instead of duplicating it"
        );

        // A later sweep that re-reads the row observes the new value and must
        // still be refused while the slot is in the future.
        let reloaded = status_monitors::Entity::find_by_id(monitor.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("monitor still exists");
        assert!(
            HealthCheckService::claim_due_monitor(db.as_ref(), &reloaded).await,
            "a fresh observation of the current value is a valid CAS and re-claims"
        );
    }

    /// On-demand environments are excluded from probing (a health check
    /// through the proxy resets the idle timer and defeats scale-to-zero) —
    /// but they must still be stamped, or the sweep re-fetches the entire
    /// on-demand set four times a minute forever.
    #[tokio::test]
    async fn run_all_checks_stamps_on_demand_monitors_instead_of_refetching_them() {
        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let (project, environment) =
            seed_project_and_environment(&db, "claim-ondemand-test", true).await;

        let monitor = status_monitors::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            name: Set("on-demand monitor".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let config_service = test_config_service(&db, &test_db.database_url);
        let job_queue: Arc<dyn temps_core::JobQueue> = Arc::new(NeverJobQueue);
        let service = HealthCheckService::new(db.clone(), config_service, job_queue)
            .expect("test HTTP client should build");

        service
            .run_all_checks()
            .await
            .expect("sweep with only an on-demand monitor should succeed");

        let reloaded = status_monitors::Entity::find_by_id(monitor.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("monitor still exists");
        assert!(
            reloaded.next_check_at.is_some(),
            "an on-demand monitor must be stamped even though it is not probed"
        );

        // Not probed: no status_checks row was written for it.
        let checks = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert!(
            checks.is_empty(),
            "on-demand environments must never be probed by the sweep"
        );

        let due_ids: Vec<i32> = HealthCheckService::due_monitors_query(Utc::now())
            .all(db.as_ref())
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert!(
            !due_ids.contains(&monitor.id),
            "the on-demand monitor must drop out of the next sweep's result set"
        );
    }
}
