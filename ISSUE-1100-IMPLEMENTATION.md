# Issue #1100: Probe automatic monitors without depending on public DNS

Issue: https://github.com/gotempsh/temps/issues/1100

## Assignment

- Branch: `fix/1100-local-monitor-probes`
- Investigated base: `origin/main` at `5045dca71` (2026-09-23).
- Deliverable: a verified fix and one focused PR targeting `main`, referencing `Fixes #1100`. Do not merge automatically.
- Follow-up correctness fixes are tracked in issue #1121 and its implementation document.

Read workspace and repository `AGENTS.md`, `CLAUDE.md`, and applicable `bugfix` / `pr-evidence` guidance. You are not alone in the workspace: preserve existing edits, including the explicitly listed work below, and coordinate overlapping files with the other issue agents. Use only this issue's worktree; the original `temps/` checkout contains unrelated dirty files and local secrets.

## Problem and reproduction

With `external_url` unset, generated environment URLs use `http://<environment-host>:<proxy-port>`. Behind a CDN/tunnel, public DNS points at the CDN, which does not serve the internal proxy port. Automatic monitors stay down although the public HTTPS site works. Setting external_url also changes container API URLs, which is not an acceptable mandatory workaround.

Reproduce using a synthetic environment hostname that does not resolve to the local listener, an internal HTTP proxy port, and a managed monitor. The fixed probe must reach the local proxy with the environment Host and correct health path without changing external_url/TEMPS_API_URL.

## Confirmed code map

- The probe implementation is `crates/temps-status-page/src/services/health_check_service.rs`, NOT the separate monitoring crate's credential HTTP checks.
- `HealthCheckService::check_monitor` obtains `ConfigService::get_deployment_url_by_slug`, builds a URL using `probe_url`, then calls its shared reqwest client directly. This invokes system/public DNS.
- `HealthCheckService::new` disables redirects intentionally to prevent app-controlled redirects from becoming SSRF. Keep that protection.
- `status_monitors::Model::is_managed` distinguishes automatic monitors.
- `crates/temps-config/src/service.rs`: `proxy_port`, `get_server_config`, `get_deployment_url_by_slug`, and server config's `address`/`tls_address` are relevant. An unspecified bind address is not a destination: translate `0.0.0.0` / `::` appropriately.
- `crates/temps-proxy/src/proxy.rs` handles HTTP→HTTPS redirection, including per-environment force_https and certificate-derived policies. A redirect counts as operational in the current health checker. Account for this explicitly when choosing local probe transport.
- Existing checks skip paused/on-demand deployments and use monitor/deployment snapshots to avoid persisting stale results. Preserve those safeguards.

## Implementation plan

1. Separate the application's logical URL/Host and health path from the transport destination.
2. For automatic monitors in the reported no-external-URL setup, route the request to the configured local proxy listener while preserving the environment's Host header. Avoid mutating installation external_url or container API URL settings.
3. Define behavior for explicitly configured public URLs and non-managed monitors. Preserve their intended semantics unless changing them is deliberate and tested; do not silently turn every external check into an internal one.
4. Normalize wildcard IPv4/IPv6 listener addresses and respect actual configured ports/bind addresses. Consider split proxy/console deployment; do not hard-code loopback if the configured proxy runs elsewhere.
5. Disable inherited HTTP proxy environment settings for local-origin traffic so it cannot accidentally leave via an external proxy. Preserve TLS verification when using TLS; do not add blanket accept-invalid-certificates.
6. Keep stored check_path validation, redirect blocking, request timeouts/retry limits, and contextual failure reporting.
7. Test force_https/redirect behavior: merely classifying a proxy-level redirect as healthy can mask an unavailable application. Choose a transport/routing method that measures the intended application availability, and document any limitation precisely.
8. Existing stored managed monitors should benefit automatically without recreation or a destructive migration. Recovery should update normal alarm state through existing paths.

## Implemented behavior

- Managed monitors use the configured local proxy listener when `external_url` is unset. The request keeps the generated environment URL, Host header, health path, and TLS hostname while reqwest pins DNS resolution to the proxy socket.
- Wildcard listeners become matching-family loopback destinations (`0.0.0.0` → `127.0.0.1`, `::` → `::1`); explicit listener IPs and configured ports are preserved.
- Local probes disable inherited HTTP proxy settings and retain redirect blocking and certificate verification.
- Manual monitors and installations with an explicit `external_url` retain their public-network behavior.
- A proxy-owned, same-host HTTP-to-HTTPS redirect on port 443 is re-probed through the configured local TLS listener. Application redirects and cross-host redirects are never followed. If HTTPS is required but no TLS listener exists, the monitor records a degraded result instead of treating a proxy-only redirect as proof that the application is healthy.
- Existing monitor rows benefit immediately; no migration or monitor recreation is required.

## Evidence

- A managed monitor with CDN/unresolvable application DNS reaches the local proxy, with the original environment Host and custom health path.
- The environment's public URL and container TEMPS_API_URL remain unchanged.
- Wildcard IPv4, IPv6, explicit listener address, and nonstandard proxy ports work.
- Unhealthy application responses still produce the correct degraded/outage state; later successful probes recover it.
- App-controlled redirects are not followed to private, metadata, or unrelated endpoints.
- Paused/on-demand behavior, check scheduling, and stale-result fencing remain covered.
- Focused real-listener regressions pass for unresolvable public DNS, custom path and Host preservation, IPv4/IPv6 wildcard normalization, external/manual compatibility, redirect blocking, and safe HTTPS-upgrade eligibility.
- `cargo test --lib -p temps-status-page -- --nocapture`: 113 passed, 0 failed.
- `cargo check --lib -p temps-status-page`: passed.
- Isolated `/start-temps` slot 4 smoke: console `/readyz` 200, proxy `/` 200, web `/` 200 on the branch build.
- Security review remains required before merge because this changes monitor transport and redirect handling.

## Merge gate

Review transport pinning, Host/path construction, redirect constraints, TLS verification, and proxy-environment isolation. Do not merge without the required security review and green CI.
