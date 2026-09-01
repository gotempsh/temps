// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A `clickhouse::Client` pointed at Temps Cloud's telemetry read proxy.
//!
//! # What this is
//!
//! Temps Cloud exposes `POST /v1/telemetry/query`: a byte-level passthrough of
//! the ClickHouse HTTP interface. It authenticates the calling instance,
//! re-authenticates upstream as that tenant's own restricted ClickHouse user,
//! refuses anything that is not a read, and streams ClickHouse's raw HTTP
//! response back unmodified. Because the response is unmodified, a stock
//! `clickhouse::Client` can be pointed straight at that URL and behave exactly
//! as if it were talking to a ClickHouse server.
//!
//! This module is the client half of that: it turns the credential
//! [`CloudLink`] already holds into a ready-to-use `clickhouse::Client`.
//!
//! # What this is deliberately *not*
//!
//! A proof-of-concept spike, and nothing more. It is not the routing seam, and
//! not the `OtelStorage`/`AnalyticsEvents` decorators, that ADR-040 describes —
//! neither exists yet and neither is built here. The source badge that ADR also
//! describes *does* exist, as
//! `web/src/components/global/TelemetrySourceBadge.tsx`, but nothing is wired to
//! it yet: it renders the source a response declares, and this module declares
//! none. Nothing in the instance calls this module either; it exists so the
//! plumbing can be proven end to end before that work starts.
//!
//! # Read-only
//!
//! The proxy rejects non-read statements with `400`. Do not attempt writes
//! through this path: local telemetry storage stays authoritative, and Cloud
//! remains a mirror that is written to only by [`CloudLink::flush`].

use std::future::Future;
use std::time::Duration;

use base64::Engine as _;

use crate::{CloudError, CloudLink, REQUEST_TIMEOUT};

/// Path of Temps Cloud's ClickHouse HTTP-interface read proxy, appended to the
/// same backend origin every other Cloud call already uses.
pub const TELEMETRY_QUERY_PATH: &str = "/v1/telemetry/query";

/// Database the Cloud-side telemetry tables live in.
///
/// **This must match whatever the Cloud side actually names that database.**
/// The schema is not in this repository, so there is nothing here that can
/// verify it. If the two ever diverge, every query fails upstream with
/// `UNKNOWN_DATABASE` and no code on this side will be able to explain why,
/// which is exactly the failure mode the live round-trip test in
/// `tests/cloud_query_proxy_live_test.rs` exists to catch.
pub const CLOUD_TELEMETRY_DATABASE: &str = "temps_cloud";

/// Username presented to the proxy.
///
/// The proxy ignores it — the HTTP Basic *password* field carries the instance
/// enrollment token, and that token alone resolves the tenant. It is a fixed,
/// descriptive placeholder rather than an empty string so that a request
/// captured in a Cloud access log is still attributable to this client.
pub const QUERY_PROXY_USER: &str = "temps-instance";

/// Wall-clock budget for a single read against the Cloud proxy.
///
/// Deliberately the *same* budget every other Cloud call already gets, taken
/// from the same constant rather than restated: a query runs alongside the
/// instance's own work, and a slow backend must never become the instance's
/// latency. There is no second number to keep in sync.
pub const QUERY_BUDGET: Duration = REQUEST_TIMEOUT;

/// Nothing on this side bounds response size. The proxy's forwarded-parameter
/// allowlist (`query`, `database`, `default_format`, `compress`,
/// `enable_http_compression`, `query_id`) has no room for a caller-supplied
/// `max_result_bytes`, so it cannot be requested here — Cloud bounds its own
/// query cost server-side (`max_rows_to_read`/`max_result_rows`/
/// `max_memory_usage`, injected unconditionally on every forwarded query). If
/// the instance ever needs its own protection against an oversized decoded
/// response, it has to be a client-side buffering limit, not a ClickHouse
/// setting sent over this wire — there is no such limit today.
///
/// Run one read against the Cloud proxy under [`QUERY_BUDGET`].
///
/// `clickhouse` 0.15 has no request or connect timeout to configure: its
/// transport is a `hyper_util` legacy client it builds internally, and the
/// `HttpClient` trait needed to substitute one is not exported. The settings
/// [`into_client`](ClickHouseQueryTarget::into_client) applies bound what
/// *ClickHouse* will spend on the query, which does nothing for a connection
/// that stalls before or after it. So the wall-clock bound has to be applied
/// here, by the caller, and every caller must apply it.
///
/// The query's own outcome passes through untouched in the inner `Result` —
/// "the statement was rejected" and "the backend never answered" are different
/// problems with different fixes, and collapsing them would cost the operator
/// the distinction.
///
/// ```no_run
/// # async fn example(link: &temps_cloud_client::CloudLink) -> Result<(), Box<dyn std::error::Error>> {
/// let client = link.clickhouse_query_client()?;
/// let count = temps_cloud_client::query::within_query_budget(
///     client.query("SELECT count() FROM telemetry_spans").fetch_one::<u64>(),
/// )
/// .await??;
/// # let _ = count;
/// # Ok(())
/// # }
/// ```
pub async fn within_query_budget<F>(query: F) -> Result<F::Output, CloudError>
where
    F: Future,
{
    within_budget(QUERY_BUDGET, query).await
}

/// [`within_query_budget`] with the budget supplied, so the timeout itself can
/// be tested without spending [`QUERY_BUDGET`] to do it.
async fn within_budget<F>(budget: Duration, query: F) -> Result<F::Output, CloudError>
where
    F: Future,
{
    tokio::time::timeout(budget, query).await.map_err(|_| {
        // `Unreachable` rather than a new variant: it is already the crate's
        // "the backend did not answer, try again later" case and is already
        // classified retryable, which a budget overrun is. Nothing is spooled
        // because nothing is buffered on a read — there is no payload to keep.
        CloudError::Unreachable {
            reason: format!(
                "the Temps Cloud telemetry read proxy at {TELEMETRY_QUERY_PATH} did not answer within {budget:?}"
            ),
            spooled_bytes: 0,
        }
    })
}

/// Everything needed to point a ClickHouse client at the Cloud read proxy.
///
/// Split out from [`CloudLink::clickhouse_query_client`] because
/// `clickhouse::Client` exposes no accessors for its URL, database or
/// credentials — without this there would be no way to assert the wiring is
/// correct short of standing up a server and watching the wire.
///
/// Nothing on it is public. The accessors exist for that assertion and are
/// therefore `#[cfg(test)]`: one of them hands back the raw instance token, and
/// a credential accessor that does not exist in a shipped build cannot become
/// the way code outside this crate reads the token out of the link. When the
/// read-routing logic needs one of them for real, un-gate that one and say why.
#[derive(Clone, PartialEq, Eq)]
pub struct ClickHouseQueryTarget {
    url: String,
    database: String,
    user: String,
    /// The instance enrollment token — the same credential `CloudClient`
    /// sends as `bearer_auth`, read from the same place. There is deliberately
    /// no second source for it.
    token: String,
}

impl std::fmt::Debug for ClickHouseQueryTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseQueryTarget")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Read-back for the wiring assertions, and only for those.
///
/// `clickhouse::Client` will not tell a test what URL, database or credential
/// it was built with, so the tests assert against this type instead. None of it
/// is compiled into a shipped build.
#[cfg(test)]
impl ClickHouseQueryTarget {
    /// Full URL of the read proxy, including the path.
    fn url(&self) -> &str {
        &self.url
    }

    /// Database name sent as the `database` query parameter.
    fn database(&self) -> &str {
        &self.database
    }

    /// Placeholder username. The proxy ignores it.
    fn user(&self) -> &str {
        &self.user
    }

    /// The raw instance enrollment token.
    fn token(&self) -> &str {
        &self.token
    }
}

impl ClickHouseQueryTarget {
    /// The `Authorization` header value the proxy authenticates against.
    ///
    /// HTTP Basic, with the instance enrollment token in the password field.
    ///
    /// This is built by hand on purpose. `clickhouse` 0.15's `with_user`/
    /// `with_password` do **not** produce Basic auth — they emit
    /// `X-ClickHouse-User` / `X-ClickHouse-Key` headers (see
    /// `clickhouse::headers::with_authentication`). Relying on them alone
    /// would send the token in a header the proxy does not read, and the
    /// request would come back `401` with nothing on this side able to say why.
    ///
    /// They are deliberately *not* set alongside this header either. That would
    /// put a second copy of a bearer-equivalent credential on every request,
    /// under a header name (`X-ClickHouse-Key`) that log, WAF and APM redaction
    /// rules key on `Authorization` and will not recognise. The only argument
    /// for sending both was that the same client would then work against a
    /// stock ClickHouse server, and it does not apply: this URL is always the
    /// Cloud proxy path, never a ClickHouse server.
    pub(crate) fn authorization_header(&self) -> String {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.user, self.token));
        format!("Basic {encoded}")
    }

    /// Build the client.
    pub fn into_client(self) -> clickhouse::Client {
        let authorization = self.authorization_header();
        clickhouse::Client::default()
            .with_url(self.url)
            .with_database(self.database)
            // Basic auth only. See `authorization_header` for why `with_user`/
            // `with_password` are not also set.
            .with_header("Authorization", authorization)
            // Deliberately NOT setting `max_execution_time`/`max_result_bytes`/
            // `result_overflow_mode` here. The proxy forwards only a fixed
            // allowlist of query parameters (`query`, `database`,
            // `default_format`, `compress`, `enable_http_compression`,
            // `query_id`) and rejects anything else with 400 — these three are
            // not on it. Cloud already appends its own `max_execution_time`,
            // `max_rows_to_read`, `max_result_rows` and `max_memory_usage`
            // unconditionally, server-side, on every forwarded query; a caller
            // cannot widen or narrow them, only [`within_query_budget`]'s
            // wall-clock bound (below) is enforceable from this side.
            // Compression is off for the spike. `compress=1` would make
            // ClickHouse frame the response body in its own codec, which only
            // survives if the proxy forwards that query parameter *and* keeps
            // the body byte-identical. Both are true by design, but neither is
            // observable from this side, and a decode failure here would look
            // identical to a broken proxy. One fewer variable between a failing
            // query and its actual cause.
            .with_compression(clickhouse::Compression::None)
    }
}

impl CloudLink {
    /// Resolve the read proxy's connection parameters from the live link.
    pub(crate) fn clickhouse_query_target(&self) -> Result<ClickHouseQueryTarget, CloudError> {
        // Same gate as every other telemetry-domain Cloud operation
        // (`record`, `pseudonymize_telemetry_id`): an operator who has switched
        // telemetry off has switched off this instance exchanging telemetry
        // with Cloud, and that has to include reading it back. Checked before
        // the credential is read, so a disabled instance never even touches the
        // token — same order as `send_notification` and
        // `linked_backup_credential`.
        if !self.telemetry_enabled() {
            return Err(CloudError::FeatureDisabled {
                feature: "telemetry",
            });
        }
        // Same credential source as every other Cloud call: no second place
        // that reads or derives the instance token, and the same three refusal
        // reasons (state unreadable / outbound blocked / not enrolled), each of
        // which needs a different fix from the operator.
        let (base_url, token) = self.linked_credential()?;
        let backend = self.parse_backend(&base_url)?;
        Ok(ClickHouseQueryTarget {
            url: backend.endpoint(TELEMETRY_QUERY_PATH).to_string(),
            database: CLOUD_TELEMETRY_DATABASE.to_string(),
            user: QUERY_PROXY_USER.to_string(),
            token,
        })
    }

    /// A `clickhouse::Client` that reads this tenant's telemetry from Temps
    /// Cloud.
    ///
    /// Returns an error rather than `None` when the link cannot serve queries,
    /// because "not enrolled", "the state file is unreadable", "outbound Cloud
    /// calls are blocked" and "telemetry is switched off" need four different
    /// fixes and an operator debugging alone cannot tell them apart from a bare
    /// `None`. This mirrors [`CloudLink::managed_ai_capability`] and every other
    /// Cloud-facing method on this type.
    ///
    /// Every read taken through the returned client must be wrapped in
    /// [`within_query_budget`] — the client itself carries no wall-clock
    /// timeout, because `clickhouse` 0.15 offers nowhere to set one.
    ///
    /// Read queries only — the proxy rejects anything else with `400`.
    pub fn clickhouse_query_client(&self) -> Result<clickhouse::Client, CloudError> {
        Ok(self.clickhouse_query_target()?.into_client())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::EnrollmentState;
    use crate::CloudFeatureSwitches;
    use std::net::SocketAddr;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use axum::{extract::State, routing::post, Router};

    const TEST_TOKEN: &str = "inst_live_token";

    /// `base64("temps-instance:inst_live_token")`, written out rather than
    /// recomputed.
    ///
    /// Recomputing it with the same expression the implementation uses would
    /// assert only that the code equals itself: swapping `STANDARD` for
    /// `STANDARD_NO_PAD` or `URL_SAFE` would change what goes on the wire and
    /// the test would still pass. The proxy accepts exactly one encoding, so
    /// the test has to name it.
    const EXPECTED_BASIC_CREDENTIAL: &str = "dGVtcHMtaW5zdGFuY2U6aW5zdF9saXZlX3Rva2Vu";

    /// Write a link state file the way `CloudLink::load_inner` expects to find
    /// it, so a test can have a linked instance without an enrollment round
    /// trip. Keep in sync with the `cloud-link/state.json` layout in `link.rs`.
    fn write_state(data_dir: &Path, base_url: &str, token: Option<&str>) {
        let mut state = EnrollmentState::new(base_url);
        state.token = token.map(str::to_string);
        state.allow_loopback_development = base_url.starts_with("http://");
        state
            .save(&data_dir.join("cloud-link").join("state.json"))
            .expect("persist link state");
    }

    /// Turn the telemetry switch on. Switches default to off, and the query
    /// path refuses while telemetry is off, so every test that expects to get
    /// past that gate has to say so explicitly.
    fn enable_telemetry(link: &CloudLink) {
        link.set_feature_switches(CloudFeatureSwitches {
            telemetry: true,
            backups: false,
            notifications: false,
        })
        .expect("apply feature switches");
    }

    fn linked(dir: &tempfile::TempDir, base_url: &str) -> CloudLink {
        write_state(dir.path(), base_url, Some(TEST_TOKEN));
        let link = CloudLink::load_for_loopback_development(dir.path().to_path_buf(), "0.1.0-test");
        enable_telemetry(&link);
        link
    }

    /// `clickhouse::Client` is not `Debug`, so `unwrap_err` is unavailable.
    fn refusal(link: &CloudLink) -> CloudError {
        match link.clickhouse_query_client() {
            Ok(_) => panic!("expected the link to refuse to build a query client"),
            Err(error) => error,
        }
    }

    #[test]
    fn an_unconfigured_instance_says_it_is_not_enrolled() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = CloudLink::load(dir.path().to_path_buf(), "0.1.0-test");
        enable_telemetry(&link);

        // Not "None". An operator who has never linked needs to be told to
        // link, and that is a different message from every other refusal.
        assert!(matches!(refusal(&link), CloudError::NotEnrolled));
    }

    #[test]
    fn a_configured_but_unenrolled_instance_is_still_not_enrolled() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_state(dir.path(), "https://cloud.example.test", None);
        let link = CloudLink::load(dir.path().to_path_buf(), "0.1.0-test");
        enable_telemetry(&link);

        assert!(matches!(refusal(&link), CloudError::NotEnrolled));
    }

    #[test]
    fn blocked_outbound_calls_are_refused_with_their_reason() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = linked(&dir, "https://cloud.example.test");
        link.block_outbound("link settings failed to persist");

        match refusal(&link) {
            CloudError::ConfigurationBlocked { reason } => {
                assert!(reason.contains("persist"), "lost the reason: {reason}");
            }
            other => panic!("expected the block to be reported, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_switched_off_refuses_before_reading_the_credential() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = linked(&dir, "https://cloud.example.test");
        link.set_feature_switches(CloudFeatureSwitches::default())
            .expect("apply feature switches");

        // A fully linked, unblocked instance — the only thing standing between
        // it and Cloud is the operator's own switch, and that has to be enough.
        match refusal(&link) {
            CloudError::FeatureDisabled { feature } => assert_eq!(feature, "telemetry"),
            other => panic!("expected the telemetry switch to refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_linked_instance_targets_the_cloud_read_proxy() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = linked(&dir, "https://cloud.example.test");

        let target = link
            .clickhouse_query_target()
            .expect("a linked instance can build a query target");

        assert_eq!(
            target.url(),
            "https://cloud.example.test/v1/telemetry/query"
        );
        assert_eq!(target.database(), "temps_cloud");
        assert_eq!(target.user(), "temps-instance");
        // The instance token, in the Basic password field — the whole point of
        // the scheme is that no new credential is introduced.
        assert_eq!(target.token(), TEST_TOKEN);

        assert_eq!(
            target.authorization_header(),
            format!("Basic {EXPECTED_BASIC_CREDENTIAL}"),
            "the proxy accepts exactly one encoding of the Basic credential"
        );
    }

    #[test]
    fn the_token_never_appears_in_debug_output() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = linked(&dir, "https://cloud.example.test");
        let rendered = format!("{:?}", link.clickhouse_query_target().expect("target"));

        assert!(
            !rendered.contains(TEST_TOKEN),
            "the instance token leaked into Debug output: {rendered}"
        );
    }

    #[test]
    fn a_query_gets_the_same_budget_as_every_other_cloud_call() {
        // Not a restatement of the constant: the point is that there is one
        // number, and that a future edit to `REQUEST_TIMEOUT` moves this too.
        assert_eq!(QUERY_BUDGET, crate::REQUEST_TIMEOUT);
    }

    #[tokio::test]
    async fn a_read_that_never_answers_is_abandoned_at_the_budget() {
        // `clickhouse` 0.15 has no timeout to configure, so this wrapper is the
        // only thing standing between a stalled proxy and an instance request
        // that hangs forever. A tiny budget so the test does not spend the real
        // one proving it.
        let error = within_budget(Duration::from_millis(20), std::future::pending::<()>())
            .await
            .expect_err("a future that never resolves must hit the budget");

        match &error {
            CloudError::Unreachable { reason, .. } => {
                assert!(
                    reason.contains(TELEMETRY_QUERY_PATH),
                    "the refusal must name what did not answer: {reason}"
                );
                assert!(
                    reason.contains("20ms"),
                    "the refusal must name the budget it spent: {reason}"
                );
            }
            other => panic!("expected an unreachable-backend refusal, got {other:?}"),
        }
        assert!(
            error.is_retryable(),
            "a backend that was slow once may answer next time"
        );
    }

    #[tokio::test]
    async fn a_read_inside_the_budget_passes_its_own_outcome_through() {
        // Both halves matter: the wrapper must not swallow the answer, and it
        // must not reinterpret the query's own failure as a Cloud failure.
        let ok: Result<u8, &str> = within_query_budget(async { Ok(7u8) })
            .await
            .expect("a ready future is inside any budget");
        assert_eq!(ok, Ok(7));

        let rejected: Result<u8, &str> = within_query_budget(async { Err("not a read") })
            .await
            .expect("a query that fails fast is still inside the budget");
        assert_eq!(rejected, Err("not a read"));
    }

    #[derive(Clone, Default)]
    struct Captured {
        uri: Arc<Mutex<Option<String>>>,
        authorization: Arc<Mutex<Option<String>>>,
        clickhouse_user: Arc<Mutex<Option<String>>>,
        clickhouse_key: Arc<Mutex<Option<String>>>,
        body: Arc<Mutex<Option<String>>>,
    }

    fn header(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    fn taken(slot: &Arc<Mutex<Option<String>>>) -> Option<String> {
        slot.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A `SELECT 1` answer in `RowBinaryWithNamesAndTypes`, the format the
    /// client asks for when validation is on: one column named `1` of type
    /// `UInt8`, then one row holding `1`.
    const SELECT_ONE_ROW_BINARY: &[u8] = &[
        0x01, // column count
        0x01, b'1', // column name "1"
        0x05, b'U', b'I', b'n', b't', b'8', // column type "UInt8"
        0x01, // the row
    ];

    /// A loopback stand-in for the Cloud proxy. No network beyond localhost —
    /// this asserts the shape of the request the proxy will actually receive,
    /// which is the part that cannot be checked by reading `clickhouse`'s
    /// builder API.
    ///
    /// `None` when the sandbox denies the bind, matching every other network
    /// test in this crate: a sandbox that forbids listening is not a defect in
    /// the code under test, and failing there would train people to ignore it.
    async fn serve(captured: Captured) -> Option<String> {
        let app = Router::new()
            .route(
                TELEMETRY_QUERY_PATH,
                post(
                    |State(c): State<Captured>,
                     uri: axum::http::Uri,
                     headers: axum::http::HeaderMap,
                     body: String| async move {
                        *c.uri.lock().unwrap_or_else(|p| p.into_inner()) = Some(uri.to_string());
                        *c.authorization.lock().unwrap_or_else(|p| p.into_inner()) =
                            header(&headers, "authorization");
                        *c.clickhouse_user.lock().unwrap_or_else(|p| p.into_inner()) =
                            header(&headers, "x-clickhouse-user");
                        *c.clickhouse_key.lock().unwrap_or_else(|p| p.into_inner()) =
                            header(&headers, "x-clickhouse-key");
                        *c.body.lock().unwrap_or_else(|p| p.into_inner()) = Some(body);
                        SELECT_ONE_ROW_BINARY
                    },
                ),
            )
            .with_state(captured);

        let listener = match tokio::net::TcpListener::bind::<SocketAddr>(
            "127.0.0.1:0".parse().expect("loopback address"),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping Cloud query proxy stub test: sandbox denied TCP bind");
                return None;
            }
            Err(error) => panic!("bind loopback listener: {error}"),
        };
        let addr = listener.local_addr().expect("listener address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some(format!("http://{addr}"))
    }

    #[tokio::test]
    async fn the_client_speaks_the_clickhouse_http_interface_at_the_proxy_path() {
        let captured = Captured::default();
        let Some(url) = serve(captured.clone()).await else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let link = linked(&dir, &url);

        let value = within_query_budget(
            link.clickhouse_query_client()
                .expect("a linked instance can build a client")
                .query("SELECT 1")
                .fetch_one::<u8>(),
        )
        .await
        .expect("the stub answers well inside the budget")
        .expect("the round trip should decode");

        assert_eq!(value, 1, "the typed result must survive the round trip");

        let uri = taken(&captured.uri).expect("the proxy path should have been hit");
        assert!(
            uri.starts_with("/v1/telemetry/query?"),
            "the query must be POSTed to the proxy path, got {uri}"
        );
        assert!(
            uri.contains("database=temps_cloud"),
            "the Cloud telemetry database must be selected, got {uri}"
        );
        assert!(
            !uri.contains("max_execution_time")
                && !uri.contains("max_result_bytes")
                && !uri.contains("result_overflow_mode"),
            "these are not on the proxy's forwarded-parameter allowlist and \
             would get the request rejected with 400, got {uri}"
        );

        assert_eq!(
            taken(&captured.authorization),
            Some(format!("Basic {EXPECTED_BASIC_CREDENTIAL}")),
            "the proxy authenticates via Basic auth with the instance token as the password"
        );

        // One copy of the credential on the wire, not two. `X-ClickHouse-Key`
        // is a bearer-equivalent secret under a header name that redaction
        // rules keyed on `Authorization` do not know about, and the proxy never
        // reads it, so sending it would be pure exposure.
        assert_eq!(
            taken(&captured.clickhouse_user),
            None,
            "the request must not carry a second, unredacted copy of the credential"
        );
        assert_eq!(
            taken(&captured.clickhouse_key),
            None,
            "the request must not carry a second, unredacted copy of the credential"
        );

        assert_eq!(
            taken(&captured.body),
            Some("SELECT 1".to_string()),
            "the SQL travels in the request body, as the HTTP interface expects"
        );
    }
}
