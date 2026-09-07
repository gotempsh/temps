// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Cloud telemetry read proxy, against a really deployed Temps Cloud.
//!
//! This is the one test that can prove the spike: everything else in this
//! crate stubs the backend, so it can verify the request this instance *sends*
//! but never that Temps Cloud accepts it, authenticates it, forwards it to
//! ClickHouse and streams a decodable answer back.
//!
//! It needs a real deployed backend and a real instance credential, so it is
//! `#[ignore]`d — the same convention `temps-presets`'
//! `every_starter_builds_runs_and_serves` uses for the tests that need
//! infrastructure CI does not have. `cargo test` skips it; nothing here runs
//! by default.
//!
//! # Running it
//!
//! ```text
//! export TEMPS_CLOUD_QUERY_PROXY_TEST_URL=https://<the-cloud-backend-origin>
//! export TEMPS_CLOUD_QUERY_PROXY_TEST_TOKEN=<an instance enrollment token>
//!
//! cargo test -p temps-cloud-client --test cloud_query_proxy_live_test -- --ignored --nocapture
//! ```
//!
//! The token is the same credential `CloudClient` already sends as
//! `bearer_auth` for `POST /v1/telemetry` ingest. On a linked instance it is
//! the `token` field of `<TEMPS_DATA_DIR>/cloud-link/state.json` (plaintext
//! state), or whatever the enrollment that linked it returned. No new
//! credential exists for this path, by design.

use temps_cloud_client::{
    query::within_query_budget, CloudFeatureSwitches, CloudLink, EnrollmentState,
};

const URL_VAR: &str = "TEMPS_CLOUD_QUERY_PROXY_TEST_URL";
const TOKEN_VAR: &str = "TEMPS_CLOUD_QUERY_PROXY_TEST_TOKEN";

/// Read the two variables, naming precisely which one is missing.
///
/// A lone operator running this against a freshly deployed proxy needs to know
/// *which* half of the configuration is absent; "not configured" would send
/// them looking in both places.
fn configuration() -> (String, String) {
    let url = match std::env::var(URL_VAR) {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "{URL_VAR} is not set to a Temps Cloud backend origin.\n\n\
             This test talks to a really deployed read proxy; there is nothing to \
             point it at otherwise. Set both variables and re-run with --ignored:\n  \
             export {URL_VAR}=https://<the-cloud-backend-origin>\n  \
             export {TOKEN_VAR}=<an instance enrollment token>"
        ),
    };
    let token = match std::env::var(TOKEN_VAR) {
        Ok(token) if !token.trim().is_empty() => token,
        _ => panic!(
            "{TOKEN_VAR} is not set.\n\n\
             It is the instance enrollment token this instance already uses for \
             `POST /v1/telemetry` ingest — on a linked instance, the `token` field \
             of <TEMPS_DATA_DIR>/cloud-link/state.json. There is no separate \
             credential for the query proxy."
        ),
    };
    (url, token)
}

/// A linked instance pointed at the configured backend.
///
/// The state file is written directly rather than enrolled, because
/// enrollment needs an operator-pasted, single-use code and this test has to be
/// re-runnable. The path mirrors `CloudLink::load_inner`'s
/// `<data_dir>/cloud-link/state.json`.
fn linked_instance(dir: &tempfile::TempDir, base_url: &str, token: &str) -> CloudLink {
    let mut state = EnrollmentState::new(base_url);
    state.token = Some(token.to_string());
    state.allow_loopback_development = base_url.starts_with("http://");
    state
        .save(&dir.path().join("cloud-link").join("state.json"))
        .expect("persist the link state the test fixture depends on");

    let link = CloudLink::load_for_loopback_development(
        dir.path().to_path_buf(),
        "0.1.0-query-proxy-spike",
    );
    // Feature switches default to off and the query path refuses while
    // telemetry is off, exactly as it does for an operator who has switched it
    // off. A linked state file alone is not enough.
    link.set_feature_switches(CloudFeatureSwitches {
        telemetry: true,
        backups: false,
        notifications: false,
    })
    .expect("enable telemetry for the fixture");
    link
}

#[tokio::test]
#[ignore = "needs a deployed Temps Cloud read proxy and an instance token; run explicitly with --ignored"]
async fn a_trivial_read_round_trips_through_the_cloud_proxy() {
    let (url, token) = configuration();
    let dir = tempfile::tempdir().expect("temp dir");
    let link = linked_instance(&dir, &url, &token);

    let client = link
        .clickhouse_query_client()
        .expect("a linked instance must be able to build a query client");

    // `SELECT 1` deliberately touches no data. What is under test is the
    // plumbing — instance credential accepted, tenant resolved, query forwarded
    // to ClickHouse as the tenant's restricted user, raw HTTP response streamed
    // back intact and decoded by a stock `clickhouse::Client`. If the schema,
    // the table name or the data are wrong, that is a separate failure worth
    // seeing separately.
    // `within_query_budget` is not optional garnish: `clickhouse` 0.15 has no
    // request timeout to configure, so without it a proxy that accepts the
    // connection and then stalls hangs this call forever.
    let value = within_query_budget(client.query("SELECT 1").fetch_one::<u8>())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the read proxy at {url}/v1/telemetry/query never answered: {error}\n\n\
                 The connection was accepted but nothing came back inside the budget every \
                 other Cloud call gets. Check that the proxy is actually forwarding to \
                 ClickHouse rather than blocking."
            )
        })
        .unwrap_or_else(|error| {
            panic!(
                "the read proxy at {url}/v1/telemetry/query did not return a decodable \
                 result: {error}\n\n\
                 A 401/403 means the instance token was not accepted (the proxy reads it \
                 from the HTTP Basic *password* field). A 400 means the statement was not \
                 recognised as a read, or that the tenant's ClickHouse user is `readonly=1` \
                 and may not set `max_execution_time`/`max_result_bytes`. A decode failure \
                 means the response was not passed through byte-for-byte."
            )
        });

    assert_eq!(
        value, 1,
        "the proxy must return ClickHouse's answer unmodified"
    );
}
