# Application Runtime Contract

Read this file whenever preparing or reviewing an application that will run on Temps. It owns application behavior; use `temps-cli` for the commands that create projects, deploy images, or manage resources.

## Deployment source and effective application root

For a repository build, place `.temps.yaml` in the application's effective Temps **Root Directory / Docker build context**. That is the repository root for a single-app repository, but it is usually an app subdirectory in a monorepo:

```text
repository/
├── apps/
│   └── api/
│       ├── .temps.yaml
│       ├── package.json
│       └── src/
└── packages/
```

If the Temps project Root Directory is `apps/api`, the health configuration belongs in `apps/api/.temps.yaml`.

Merge known keys instead of replacing the file. Preserve existing `build`, `cron`, `env`, `agents`, `workflows`, `sourceContext`, and health settings.

Image and static deployments do not have repository contents from which Temps can read `.temps.yaml`. Configure their health route through the deployment request's `health_check_path` or the CLI `--health-check-path` option.

## Deployment health

Create a dedicated unauthenticated `GET` endpoint such as `/healthz` and configure it:

```yaml
health:
  path: /healthz
```

Current implementation facts:

- `health.path` is the only `.temps.yaml` health field that reliably changes deployment and monitor behavior today.
- Do not add or rely on `health.status`, `health.interval`, `health.timeout`, or `health.retries` until their runtime consumers are implemented.
- A deploy-time `--health-check-path` override wins over `.temps.yaml`. Avoid an accidental override for repository builds; supply it deliberately for image/static deployments.
- Temps configuration parsing does not reject every unknown key, and an unreadable/invalid file can be treated as absent. Verify the deployment log names the intended health path and confirm the environment's uptime monitor uses it.

Temps uses the endpoint for deployment health and monitoring. It is not a continuous live-traffic gate, so returning `503` does not by itself remove a running container from routing:

- Return `200` only when the process can serve normal requests.
- Return `503` while starting or missing a dependency required by most requests. It may also describe a draining process while the endpoint remains reachable, but do not rely on that response to stop new traffic.
- Check only essential dependencies, use short timeouts, and never mutate state.
- Return a constant minimal body such as `{"status":"ok"}`. Do not expose hostnames, versions, commit SHAs, dependency errors, configuration, or secrets.

Temps is permissive about some non-5xx responses during probing, so an explicit `200` is important: it prevents a typo that returns `404` from looking intentionally healthy.

For OpenTelemetry-enabled applications, exclude the exact configured health path from incoming server spans. Suppress routine health requests from access logs and request metrics where the framework supports it. Keep the application route, repository `.temps.yaml` or deployment override, and filters synchronized.

### Scale-to-zero caveat

The scale-to-zero wake readiness probe currently checks `/` independently of `.temps.yaml`. Excluding `/` would hide legitimate homepage traffic, so do not broaden the filter to `/`. Treat remaining wake-probe spans as a platform limitation until Temps uses the configured health path for wake readiness.

## Network binding and port alignment

Temps injects `HOST=0.0.0.0` and `PORT`. The application must:

- Read `PORT` instead of hardcoding a framework default.
- Bind to `HOST` or `0.0.0.0`, never only `127.0.0.1`/`localhost`.
- Keep the server's listening port, project port, and Docker `EXPOSE` aligned. For custom images, a conflicting exposed port can make the proxy route to a different port than the process reads from `PORT`.

`EXPOSE` is metadata, not a substitute for listening on the injected port.

## Graceful shutdown

Temps stops containers with the normal termination signal and a 10-second grace period. On `SIGTERM`:

1. Stop accepting new requests and jobs; do not assume a health-response change removes the container from routing.
2. Let in-flight requests finish within a bounded deadline.
3. Close database, queue, and cache clients.
4. Flush and shut down OpenTelemetry processors/exporters.
5. Exit successfully before the 10-second deadline.

Use an exec-form container command, or `exec` from an entrypoint script, so the application receives signals directly.

## Replica-safe state

Each replica has private memory and a private writable filesystem. Store shared state in the appropriate service:

- Sessions, locks, rate-limit state, and cross-replica coordination: database or Redis.
- User uploads and durable artifacts: blob/S3 storage.
- Cross-replica realtime broadcasts: shared pub/sub.

Do not depend on in-memory sessions, local uploads, or a single process receiving every request.

## Database migrations

`.temps.yaml` has no general pre-deploy/release hook. Keep operational commands in `temps-cli`, but apply these application rules:

- Make migrations idempotent and concurrency-safe; multiple replicas can start concurrently.
- Prefer a one-off release/CI step when the workflow supports it.
- If startup runs migrations, use a database lock and fail startup when migration fails.
- Use backward-compatible expand/contract changes because rolling back application code does not roll back the database automatically.
- Never run migrations from the health endpoint.

## Logs and cron endpoints

Write application logs to stdout/stderr so Temps can capture them. Use OTLP logs when structured trace correlation is required; do not depend on container-local log files.

If `.temps.yaml` defines `cron` routes, validate `Authorization: Bearer <CRON_SECRET>` in every handler and fail closed when either the environment value or header is missing or malformed. Use a vetted constant-time comparison helper that safely handles unequal lengths. Keep handlers idempotent and return a non-success response when authentication or work fails.

Treat `CRON_SECRET` as a high-impact server credential: Temps currently supplies the deployment token, which is non-expiring and broadly scoped. Compare the presented value without echoing it, never record the authorization header in logs/traces/errors, and rotate the deployment credential after suspected exposure.

## Runtime verification

Before considering the app deployable:

1. Confirm the server listens on the injected `PORT` on `0.0.0.0`.
2. Confirm a repository build reads `.temps.yaml` from the effective application root, or an image/static deployment receives the intended health-path override; verify the deploy log uses that path.
3. Confirm the health route returns `200`, returns no sensitive details, and produces no routine trace/log/metric noise.
4. Send `SIGTERM`; confirm the process drains, flushes telemetry, and exits within 10 seconds.
5. Exercise two replicas or reason explicitly about every mutable in-memory/local-file dependency.
6. Confirm migrations cannot race and a rollback remains schema-compatible.
