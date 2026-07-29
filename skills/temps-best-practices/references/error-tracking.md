# Error Tracking

For step-by-step SDK init per language/framework, use the **add-error-tracking** skill — it covers Next.js, React, Vue, Svelte, Angular, Node, React Native, Python, Go, Rust, Ruby, Java, PHP, .NET, Flutter with copy-paste snippets. This file covers the ingestion internals worth knowing when debugging or reviewing that setup.

**Apps deployed on Temps get the DSN for free, with no build-time-vs-runtime gap**: every deployment automatically has `SENTRY_DSN` injected — and, when the build preset needs one, the framework-specific public-prefixed variant (`NEXT_PUBLIC_SENTRY_DSN`, `VITE_SENTRY_DSN`, etc.) too. Temps injects both as a Docker `--build-arg` *and* as a container runtime env var (`temps-deployments::workflow_planner::gather_environment_variables`, threaded to `docker.rs`'s `buildargs` and `env` respectively), so client bundlers that inline public env vars at build time see the real value — there's no scenario where the DSN "isn't available yet" during the build. No dashboard copy-paste needed, and never hardcode a DSN or ask the user for one to hardcode — see the top-level [SKILL.md](../SKILL.md) quickstart for the exact framework mapping and the scope of this auto-injection (it doesn't cover apps built outside Temps' own deploy pipeline, including Temps' own console).

## How it works

Temps is Sentry wire-compatible: it implements Sentry's ingestion protocol server-side, so the official Sentry SDK for any platform works unmodified against a Temps DSN. There is no Temps-specific error-tracking SDK.

DSN format:

```
https://<public_key>@<temps-host>/<project_id>
```

The `public_key` is per-project and derived from the DB; the `secret_key` (if any) is never exposed in API responses.

The DSN public key is intentionally safe for browser write-only ingestion. It is not equivalent to a `dt_` deployment token or `tk_` API key, which are server-only machine credentials.

## Release auto-injection

Temps also auto-injects `SENTRY_RELEASE` (the deployment's commit SHA) alongside `SENTRY_DSN` for apps deployed through Temps (`workflow_planner.rs:1066-1070,1895-1920`) — every official Sentry SDK reads `SENTRY_RELEASE` from the environment when `release` isn't explicitly set in `Sentry.init()`, tagging events with the correct release automatically. **Don't hardcode a `release` value in `Sentry.init()`** — doing so overrides the injected value and breaks the dashboard's ability to join a stack frame back to its source (**Error Tracking → Source Context**). Leave `release` unset and let the SDK pick up `SENTRY_RELEASE` on its own.

## Ingestion endpoints

Routes are registered as `/{project_id}/store/` etc. inside the crate, but the console server nests every plugin's routes under `/api` (`temps-cli/src/commands/serve/console.rs`), so the real externally-reachable paths are:

- `POST /api/{project_id}/store/` — JSON event ingestion (legacy Sentry envelope-less format)
- `POST /api/{project_id}/envelope/` — binary/newline-delimited envelope ingestion (what modern Sentry SDKs use by default)
- `POST /api/0/projects/{org_slug}/{project_slug}/releases/{version}/files/` — source map upload, `sentry-cli`-compatible (multipart form: `file`, `name`, `dist`, `header`)

This matches standard Sentry SDK behavior unmodified: every conforming Sentry SDK inserts `api/` itself when building the request URL from a DSN, so a DSN of the form `https://<public_key>@<temps-host>/<project_id>` (no `/api` in the DSN) already produces exactly these URLs — nothing Temps-specific to configure. The `/api` prefix only matters if hand-rolling a raw HTTP request (e.g. `curl`) to test ingestion without going through a real SDK.

Implemented in `temps-error-tracking` crate: `src/sentry/handlers.rs`, `src/sentry/service.rs`, `src/sentry/envelope.rs`.

## Auth

Any of these are accepted, in priority order:
- `X-Sentry-Auth: Sentry sentry_key=<public_key>,sentry_version=7` header (what Sentry SDKs send by default)
- `Authorization: Bearer <public_key>` or `Authorization: DSN <token>`
- `sentry_key` query parameter (fallback, used by some old SDKs)
- Deployment tokens (`dt_*`) are also accepted and hash-validated against the DB

## Limits

- Request body: 2 MiB compressed, 10 MiB decompressed (`SENTRY_INGEST_BODY_LIMIT`) — guards against decompression bombs.
- gzip `Content-Encoding` is supported and decoded with a size guard.

## Sensitive data

Temps stores the event the SDK sends, so scrub it before export:

- Keep legacy `sendDefaultPii` false/undefined, or use the SDK's current granular `dataCollection` allow/deny configuration.
- Do not collect authorization/cookie headers, HTTP bodies, sensitive query parameters, database parameter values, passwords, tokens, payment data, or raw GenAI inputs/outputs by default.
- Use `beforeSend` (or the language SDK's equivalent event processor) to remove unsafe request, user, breadcrumb, context, and extra fields after integrations have populated the event.
- Prefer allowlisting required fields. Returning `null` from `beforeSend` should drop an event that cannot be made safe.
- Test with synthetic canary secrets and inspect the stored event in Temps.

See [telemetry-hygiene.md](telemetry-hygiene.md) for the cross-pillar policy.

## Response shape

- Store/Envelope: `{"id": "<event_uuid>"}`, HTTP 200
- Source maps: `{"id": ..., "name": ..., "dist": ..., "headers": {...}, "size": ..., "sha1": ..., "date_created": ...}`

## Gotchas

- **Nothing shows up**: the SDK must be initialized before any code that could throw — for Node, `Sentry.init` must be the first import in the entrypoint, not somewhere mid-file.
- **Browser apps not reporting**: the DSN env var needs the bundler's public prefix (`NEXT_PUBLIC_`, `VITE_`, `PUBLIC_`) to reach the client bundle; a plain `SENTRY_DSN` won't be inlined into browser code.
- **Minified stack traces**: upload source maps via `sentry-cli sourcemaps upload` in CI, or through **Error Tracking → Source Maps** in the dashboard — the release version passed to the SDK must match the release the source maps were uploaded under.
- **Production events missing but local ones work**: the deployment's env vars are separate from local `.env` files — confirm the DSN is actually set in the deployed environment, not just locally.
- **Unexpected PII**: inspect the SDK's resolved data-collection settings and the post-integration `beforeSend` event; initializing the SDK with defaults does not prove application-added extras/breadcrumbs are safe.
