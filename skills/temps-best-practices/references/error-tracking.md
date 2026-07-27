# Error Tracking

For step-by-step SDK init per language/framework, use the **add-error-tracking** skill — it covers Next.js, React, Vue, Svelte, Angular, Node, React Native, Python, Go, Rust, Ruby, Java, PHP, .NET, Flutter with copy-paste snippets. This file covers the ingestion internals worth knowing when debugging or reviewing that setup.

**Apps deployed on Temps get the DSN for free**: every deployment automatically has `SENTRY_DSN` injected (plus a framework-specific public-prefixed variant when the build preset needs one — see the top-level [SKILL.md](../SKILL.md) quickstart for the exact mapping). No dashboard copy-paste needed for those apps.

## How it works

Temps is Sentry wire-compatible: it implements Sentry's ingestion protocol server-side, so the official Sentry SDK for any platform works unmodified against a Temps DSN. There is no Temps-specific error-tracking SDK.

DSN format:

```
https://<public_key>@<temps-host>/<project_id>
```

The `public_key` is per-project and derived from the DB; the `secret_key` (if any) is never exposed in API responses.

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

## Response shape

- Store/Envelope: `{"id": "<event_uuid>"}`, HTTP 200
- Source maps: `{"id": ..., "name": ..., "dist": ..., "headers": {...}, "size": ..., "sha1": ..., "date_created": ...}`

## Gotchas

- **Nothing shows up**: the SDK must be initialized before any code that could throw — for Node, `Sentry.init` must be the first import in the entrypoint, not somewhere mid-file.
- **Browser apps not reporting**: the DSN env var needs the bundler's public prefix (`NEXT_PUBLIC_`, `VITE_`, `PUBLIC_`) to reach the client bundle; a plain `SENTRY_DSN` won't be inlined into browser code.
- **Minified stack traces**: upload source maps via `sentry-cli sourcemaps upload` in CI, or through **Error Tracking → Source Maps** in the dashboard — the release version passed to the SDK must match the release the source maps were uploaded under.
- **Production events missing but local ones work**: the deployment's env vars are separate from local `.env` files — confirm the DSN is actually set in the deployed environment, not just locally.
