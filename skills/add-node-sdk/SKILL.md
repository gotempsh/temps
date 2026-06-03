---
name: add-node-sdk
description: |
  Integrate the Temps Node.js SDKs for server-side platform access, KV storage, and Blob storage. Use when the user wants to: (1) Call the Temps platform API from Node.js (deployments, projects, analytics, session replay, etc.), (2) Use Temps KV (key-value) storage, (3) Use Temps Blob storage for files, (4) Server-side integration with a Temps project, (5) Backend access to Temps resources. Triggers: "temps node sdk", "temps kv", "temps blob", "backend integration", "node.js temps", "@temps-sdk/node-sdk".
---

# Add Node.js SDKs

Integrate Temps platform features in Node.js / TypeScript apps.

> **Verified against the real published packages.** A prior version of this skill referenced `@temps-sdk/node` (which does not exist), `new Temps({ apiKey, projectId })`, `temps.track()`, `new KV({ apiKey, namespace })`, `kv.has()`, `blob.get()`, `blob.getSignedUrl()`, `TempsError` — **none of those exist**. Confirm any API before changing it:
> ```bash
> npm pack @temps-sdk/node-sdk@latest @temps-sdk/kv@latest @temps-sdk/blob@latest
> for t in temps-sdk-*-*.tgz; do tar -xzf "$t"; echo "== $t =="; cat package/dist/index.d.ts | head -60; rm -rf package; done
> ```

## The three real packages

| Package | Purpose |
|---|---|
| `@temps-sdk/node-sdk` | Full platform API client (generated) + Sentry-style server error tracking |
| `@temps-sdk/kv` | Vercel-KV-style key/value store |
| `@temps-sdk/blob` | Vercel-Blob-style file store |

> There is **no** `@temps-sdk/node`. The platform client package is `@temps-sdk/node-sdk`.

```bash
npm install @temps-sdk/node-sdk   # platform API + error tracking
npm install @temps-sdk/kv         # optional: KV storage
npm install @temps-sdk/blob       # optional: blob storage
```

## Platform API client — `@temps-sdk/node-sdk`

`TempsClient` is a generated client (hey-api) over the Temps OpenAPI surface. Construct it with a `baseUrl` and `apiKey`, then call resource sub-namespaces.

```typescript
import { TempsClient } from '@temps-sdk/node-sdk';

const temps = new TempsClient({
  baseUrl: process.env.TEMPS_API_URL!,   // e.g. https://app.temps.sh
  apiKey: process.env.TEMPS_API_KEY,     // create under Settings → API Keys
});

// Resource namespaces (each maps to OpenAPI operations):
// temps.projects, temps.deployments, temps.analytics, temps.sessionReplay,
// temps.domains, temps.dns, temps.backups, temps.crons, temps.email,
// temps.externalServices, temps.files, temps.funnels, temps.git, temps.monitoring,
// temps.notifications, temps.performance, temps.platform, temps.proxyLogs,
// temps.repositories, temps.settings, temps.users, temps.apiKeys, temps.auditLogs, …

const projects = await temps.projects /* .list(...) etc. */;
```

> ⚠️ The config is `{ baseUrl, apiKey }` (Vercel-style `baseUrl`), **not** `{ apiKey, projectId }`. There is **no** top-level `temps.track()` / `temps.identify()` — analytics live under `temps.analytics.*`. Use editor autocomplete on the namespace, or read `package/dist/index.d.ts` and `client/sdk.gen.d.ts` for exact method names and argument shapes (they're generated and may evolve).

Each call resolves to `{ data, error, request, response }` (hey-api result shape) — check `error` before using `data`.

## Server-side error tracking — `@temps-sdk/node-sdk`

The node SDK re-exports a **Sentry-compatible** error-tracking API as the `ErrorTracking` namespace. It is DSN-based and mirrors `@sentry/node`.

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({
  dsn: process.env.SENTRY_DSN!,            // Temps DSN, from Error Tracking → DSN & Setup
  environment: process.env.NODE_ENV,
  release: process.env.GIT_SHA,
  tracesSampleRate: 1.0,
});

try {
  doRiskyThing();
} catch (err) {
  ErrorTracking.captureException(err);
}

ErrorTracking.captureMessage('Something notable happened', 'warning');
ErrorTracking.setUser({ id: 'user_123', email: 'user@example.com' });
ErrorTracking.addBreadcrumb({ category: 'auth', message: 'logged in', level: 'info' });
const tx = ErrorTracking.startTransaction({ name: 'checkout', op: 'http.server' });
// … tx.startChild(...), tx.finish()
```

> For **most** error-tracking work, prefer the dedicated `add-error-tracking` skill: Temps is Sentry wire-compatible, so the official `@sentry/node` SDK pointed at a Temps DSN is the recommended path. Use `ErrorTracking` from `@temps-sdk/node-sdk` only when you specifically want the bundled implementation (no extra dependency).

## KV storage — `@temps-sdk/kv`

Vercel-KV-style API. Use the default `kv` instance (env-configured) or construct a `KV`/`createClient`.

```typescript
import { kv } from '@temps-sdk/kv';
// or: import { KV, createClient } from '@temps-sdk/kv';

// Default instance reads TEMPS_API_URL, TEMPS_TOKEN, TEMPS_PROJECT_ID from env.
await kv.set('user:123', { name: 'John' });
await kv.set('session:abc', { userId: '123' }, { ex: 3600 });  // expire in 3600s
const user = await kv.get<{ name: string }>('user:123');        // typed get; null if missing
await kv.incr('counter');
await kv.expire('user:123', 600);
const ttl = await kv.ttl('user:123');                            // seconds; -2 missing, -1 no-expiry
const removed = await kv.del('user:123', 'session:abc');         // count deleted
const matches = await kv.keys('user:*');                         // pattern match
```

Explicit client (when you don't want env-based config):

```typescript
import { KV } from '@temps-sdk/kv';

const store = new KV({
  apiUrl: process.env.TEMPS_API_URL,
  token: process.env.TEMPS_TOKEN,    // API key or deployment token
  projectId: 42,                     // number; required with API keys, inferred from deployment tokens
});
```

`SetOptions` (Redis-style): `{ ex?: number /* sec */, px?: number /* ms */, nx?: boolean, xx?: boolean }`.

> ⚠️ The config field is `token` (API key **or** deployment token) + `projectId: number`, **not** `apiKey`/`namespace`. There is **no** `kv.has()`, `kv.list({prefix})`, `getWithMetadata`, or a `{ ttl }` set option — use `{ ex }` / `{ px }`. KV errors throw `KVError`.

## Blob storage — `@temps-sdk/blob`

Vercel-Blob-style API. Use the default `blob` instance or construct `BlobClient`/`createClient`.

```typescript
import { blob } from '@temps-sdk/blob';
// or: import { BlobClient, createClient } from '@temps-sdk/blob';

// Upload (returns BlobInfo: { url, pathname, contentType, size, ... })
const info = await blob.put('avatars/user-123.png', imageBuffer, {
  contentType: 'image/png',
  addRandomSuffix: false,   // default true — set false to keep the exact pathname
});

// Download (returns a fetch Response — stream or buffer it yourself)
const res = await blob.download(info.url);
const bytes = Buffer.from(await res.arrayBuffer());

// Metadata
const meta = await blob.head(info.url);

// List with pagination
const { blobs, cursor, hasMore } = await blob.list({ prefix: 'avatars/', limit: 100 });

// Copy and delete
await blob.copy(info.url, 'avatars/backup.png');
await blob.del(info.url);            // also accepts string[] of urls/pathnames
```

Explicit client:

```typescript
import { BlobClient } from '@temps-sdk/blob';

const store = new BlobClient({
  apiUrl: process.env.TEMPS_API_URL,
  token: process.env.TEMPS_TOKEN,
  projectId: 42,
});
```

`PutOptions`: `{ contentType?, addRandomSuffix? (default true), cacheControl?, contentEncoding?, contentDisposition? }`.

> ⚠️ Methods are `put / del / head / list / download / copy`. There is **no** `blob.get()`, `blob.getStream()`, `blob.getSignedUrl()`, or `blob.createUploadUrl()` — to read content, call `download(url)` and consume the `Response`. Blob errors throw `BlobError`.

## Environment Variables

| Variable | Used by | Notes |
|---|---|---|
| `TEMPS_API_URL` | node-sdk (`baseUrl`), kv, blob | Base URL of the Temps API, e.g. `https://app.temps.sh` |
| `TEMPS_API_KEY` | node-sdk `TempsClient` | Create under **Settings → API Keys** |
| `TEMPS_TOKEN` | kv, blob | API key **or** deployment token |
| `TEMPS_PROJECT_ID` | kv, blob | Numeric project id; required with API keys, inferred from deployment tokens |
| `SENTRY_DSN` | node-sdk `ErrorTracking` | Temps DSN from **Error Tracking → DSN & Setup** |

On Temps deployments these may be injected automatically when you link a KV/Blob service to the project — check the project's environment variables before hardcoding.

## Best Practices

1. **Never hardcode keys/tokens** — read from environment variables.
2. **Check `error` on `TempsClient` calls** before using `data` (hey-api result shape).
3. **Catch `KVError` / `BlobError`** around storage calls.
4. **Initialize `ErrorTracking.init()` once** at startup, before code that can throw.
5. **Confirm method names from the generated `.d.ts`** — the platform client surface is generated from OpenAPI and evolves with the API.
