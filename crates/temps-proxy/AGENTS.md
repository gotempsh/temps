# temps-proxy

The Pingora proxy in front of every project, and its console API. Adds to
[`AGENTS.md`](../../AGENTS.md), whose hot path rules cover `LoadBalancer`.

## Owns

Per-request decisions and answers, TLS and on-demand certificates, request logs
from channel to read API, and their ClickHouse DDL. Not here:

- Routes, compiled endpoints: `temps-routes` (`CachedPeerTable`,
  `IngressSnapshot`), evaluated by `temps-workload-ingress`.
- Engines: `temps-edge-rules`, and `temps-shield`, which embeds the
  `temps-captcha-wasm` build. `src/edge_rules/`, `src/shield/` feed them.
- Edge nodes: `temps-edge`. Tunnels: `temps-ingress`. Counters:
  `temps_metrics::proxy`. Postgres schema: `temps-migrations`.

## Entry points

- `src/proxy/mod.rs`: `LoadBalancer`, `ProxyContext`, the `ProxyHttp` hooks.
- `src/proxy/request_filter/mod.rs`: `run_request_stages`, `Flow`, the stage
  list. `project.rs` beside it: `project_stage`, the per-project chain.
- `src/ingress.rs`: `legacy_access`, `decide`, `enforce_step`, `shadow_step`.
- `src/server.rs`: `setup_proxy_server`. `src/plugin.rs`: `ProxyPlugin`, console.
- `src/handler/`: axum routes, except `gate.rs`, `password_wall.rs`,
  `preview_wall.rs`, which the request path calls. `src/service/`,
  `src/storage/`: services, `proxy_log_batch_writer`, `ProxyLogStorage`.
- `migrations/clickhouse/`: SQL in `MIGRATIONS`. `password_wall/`,
  `preview_wall/`: forms compiled in. `testdata/`: the differential corpus.
  `benches/request_path.rs`: latency and allocations per way out of the hook.

## Invariants

- **Stage order** (`run_request_stages`, `project_stage`): moving a stage can
  open a bypass with no check changing. `order_tests.rs` pins pairs, not the
  list: each test sends a request two checks would answer through the real
  hooks and asserts `ctx.routing_status` and the status line. Unpinned: answers
  from `static_files_stage`, `persisted_asset_stage`, `stale_chunk_stage`,
  `late_domain_redirect_stage` and the second `acme_challenge_stage`; the place
  of `refuse_misdirected_connection`; the block list in `early_request_filter`.
- **One access answer**: legacy chain and compiled endpoint agree on the corpus,
  or changing `ingress_mode` changes responses:
  `legacy_and_compiled_endpoints_decide_every_corpus_request_alike`.
- **Compiled path allocates nothing**: `tests/ingress_no_alloc.rs` asserts zero
  across lookup, `grant_host`, `enforce_step`, `shadow_step`, and nothing else.
- **No database await per request** (top of `src/proxy/mod.rs`, no test), or a
  flood becomes a query per request. Reads are `ArcSwap` snapshots
  (`CachedPeerTable`, `ProjectIpAccessGate`, `EdgeRuleGate`, `ShieldLists`) or
  moka. `AcmeChallengeCache` misses share one rate-limited reload;
  `wake_environment` does query, behind `try_acquire_wake_slot`.
- **Writes shed, never block**: `ProxyLogBatchHandle::send_or_drop`,
  `TrackingBatchHandle::send`, `ShieldEventStore` `try_send` to bounded channels
  (`a_full_buffer_drops_instead_of_blocking`).
- **Credentials never reach storage**: `send_or_drop` redacts, pinned by
  `send_or_drop_redacts_credentials_before_they_reach_the_channel`.
- **Clients cannot pick their address**, or allow lists are bypassed:
  `remote_and_private_peers_cannot_spoof_forwarded_addresses`.
- **A failed reload keeps the last rules**, or a protected project opens:
  `hydrate_failure_is_reported_and_keeps_the_previous_snapshot`.

## Do not

- Add a stage without an order test naming what it must answer before.
- `.await` a log send or spawn `send_or_drop`: parked futures queue unbounded.
- Look up plugin services in `setup_proxy_plugins`, which has only config and
  geo. Pass them to `setup_proxy_server`, as `project_ip_gate` is.
- Vary the generic 403 and 503 bodies: they hide whether a host routes to a
  project (`access_outcome_stage`, `connection_cap_stage`).
- Reorder `MIGRATIONS`, or add a second test to `tests/ingress_no_alloc.rs`.

## Testing

`cargo test --lib -p temps-proxy <name>` or `--test ingress_no_alloc`.

- `order_tests` and the bench need no Docker: their database is a mock.
- `TestDatabase` tests in `tests.rs`, `proxy_test.rs`, `plugin.rs`,
  `e2e_static_test.rs` fail without Docker; the rest skip with a message.
- CI: `unit-b` runs the lib tests, `unit-integration` runs `ingress_no_alloc`,
  the only binary under `tests/`. Run by no group: four `known broken:` tests,
  `attribution_integration` (`--features integration`), the bench.
