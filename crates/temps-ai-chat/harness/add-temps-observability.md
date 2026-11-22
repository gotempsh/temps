---
name: add-temps-observability
description: Error tracking and traces for an app - wire them when the code already uses Sentry or OpenTelemetry, otherwise offer them
requires_tool: sandbox_exec
contexts: assistant, project
---

# Error tracking and traces

Every app Temps deploys can send errors and traces to its own project, but
only once its code uses them. Unlike visitor analytics (the `track-app-usage`
playbook, which is required), these are the user's choice.

First look at whether the code already uses them: `@sentry/` or
`@opentelemetry/` or `@vercel/otel` in `package.json`, `sentry-sdk` or
`opentelemetry` in the Python requirements, `getsentry/sentry-go` or
`go.opentelemetry.io` in `go.mod`, or files such as `instrumentation.ts`,
`instrument.*` or `sentry.*.config.*`.

- **Already there:** make sure it reads the variables below instead of a
  hardcoded DSN or endpoint, with the recipe for its stack, so it reports to
  this project. Say what you changed.
- **Not there:** do not add it. In your final message, offer it in one or two
  lines saying what the user would get: errors with their stack traces on
  the project's Errors page, and how long each request and its database and
  outside calls took on the Traces page. Add it, with the recipes below, only
  when the user says yes or asks for it. Plain HTML cannot have either, so
  offer nothing for it (see the rules).

## What Temps sets on every deployment

- `SENTRY_DSN`: a Sentry-compatible DSN for the project's environment. Temps
  speaks the Sentry protocol, so use the official Sentry SDK; there is no
  Temps error SDK.
- The browser copy of the DSN, under the prefix the preset's bundler exposes:
  `NEXT_PUBLIC_SENTRY_DSN` for `nextjs`, `VITE_SENTRY_DSN` for `vite`,
  `react` and `vue`, `PUBLIC_SENTRY_DSN` for `sveltekit` and `astro`. It is
  set at build time too, so bundlers inline it. It exists only when the
  project's preset matches the framework.
- `NEXT_PUBLIC_SENTRY_TUNNEL`, `VITE_SENTRY_TUNNEL`, `PUBLIC_SENTRY_TUNNEL`:
  a path on the app's own domain that passes browser errors to Temps. Pass it
  as `tunnel` so ad blockers and other origins do not stop them.
- `SENTRY_RELEASE`: the release. Never set `release` yourself.
- `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`
  (`http/protobuf`), `OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_SERVICE_NAME`:
  the standard OpenTelemetry exporter settings. The headers hold a deployment
  token: read them only on the server, never log them or send them to a
  browser.

None of these exist in the sandbox. Start each SDK only when its variable is
set (`enabled: !!dsn`, tracing only with an endpoint), so the app runs and
builds without them. Never hardcode, invent or ask the user for a value.

Install a recipe's packages in one `npm install`, so npm resolves the
OpenTelemetry versions they share. Never add
`--legacy-peer-deps` or pin `@opentelemetry/*` packages yourself: if the
install fails, read the error and fix the version it names.

The recipes are for Sentry SDK 11. Sentry 8 to 10 call the option
`skipOpenTelemetrySetup: true` instead of `enableOpenTelemetrySetup: false`;
read the installed `node_modules/@sentry/*/package.json` when unsure.

## Next.js (App Router)

`npm install @sentry/nextjs @vercel/otel`

`instrumentation.ts`, next to `app/` (in `src/` when the app uses `src/`):

```ts
import * as Sentry from '@sentry/nextjs'
import { registerOTel } from '@vercel/otel'

export function register() {
  Sentry.init({
    dsn: process.env.SENTRY_DSN,
    enabled: !!process.env.SENTRY_DSN,
    // Traces go to Temps through @vercel/otel. Without this, Sentry takes
    // the tracer and no span is exported.
    enableOpenTelemetrySetup: false,
    tracesSampleRate: 0,
  })
  registerOTel({
    serviceName: process.env.OTEL_SERVICE_NAME || 'web',
    // Sentry's own uploads are not the app's work.
    instrumentationConfig: { fetch: { ignoreUrls: [/\/envelope\//] } },
  })
}

export const onRequestError = Sentry.captureRequestError
```

`instrumentation-client.ts`, in the same folder:

```ts
import * as Sentry from '@sentry/nextjs'

Sentry.init({
  dsn: process.env.NEXT_PUBLIC_SENTRY_DSN,
  enabled: !!process.env.NEXT_PUBLIC_SENTRY_DSN,
  tunnel: process.env.NEXT_PUBLIC_SENTRY_TUNNEL,
  tracesSampleRate: 0,
})
```

Add `app/global-error.tsx` as a client component that calls
`Sentry.captureException(error)` in a `useEffect` and renders a short message
with a retry button. Do not wrap `next.config` in `withSentryConfig`: it is
only needed to upload source maps, and it can fail the build.

## React single-page app (Vite)

`npm install @sentry/react`

In `src/main.tsx`:

```tsx
import * as Sentry from '@sentry/react'

Sentry.init({
  dsn: import.meta.env.VITE_SENTRY_DSN,
  enabled: !!import.meta.env.VITE_SENTRY_DSN,
  tunnel: import.meta.env.VITE_SENTRY_TUNNEL,
})
```

Deploy it with the `vite` preset so `VITE_SENTRY_DSN` and `VITE_SENTRY_TUNNEL`
are set when it builds.
It has no server, so it has no traces; the API it calls traces its own
requests.

## Node server (Express, Fastify, Hono, plain `http`)

`npm install @sentry/node @opentelemetry/sdk-node @opentelemetry/exporter-trace-otlp-proto @opentelemetry/auto-instrumentations-node`

`instrument.cjs`, loaded before the app with
`node --require ./instrument.cjs server.js` in the `start` script (this works
for an ES-module server too):

```js
const Sentry = require('@sentry/node')
// Sentry 11 for Node leaves OpenTelemetry to the app, so the SDK below owns
// the tracer.
Sentry.init({
  dsn: process.env.SENTRY_DSN,
  enabled: !!process.env.SENTRY_DSN,
  tracesSampleRate: 0,
})

if (process.env.OTEL_EXPORTER_OTLP_ENDPOINT) {
  const { NodeSDK } = require('@opentelemetry/sdk-node')
  const { OTLPTraceExporter } = require('@opentelemetry/exporter-trace-otlp-proto')
  const { getNodeAutoInstrumentations } = require('@opentelemetry/auto-instrumentations-node')
  const sdk = new NodeSDK({
    traceExporter: new OTLPTraceExporter(),
    instrumentations: [
      getNodeAutoInstrumentations({
        '@opentelemetry/instrumentation-fs': { enabled: false },
        '@opentelemetry/instrumentation-http': {
          ignoreIncomingRequestHook: (req) => req.url === '/healthz',
          ignoreOutgoingRequestHook: (req) => (req.path || '').includes('/envelope/'),
        },
      }),
    ],
  })
  sdk.start()
  process.once('SIGTERM', () => sdk.shutdown().finally(() => process.exit(0)))
}
```

After the routes, hand errors to Sentry: `Sentry.setupExpressErrorHandler(app)`
for Express, `Sentry.setupFastifyErrorHandler(app)` for Fastify, and
`Sentry.captureException(err)` in `app.onError` for Hono. Keep the health
path in the ignore hook equal to the app's real health route.

## Python (FastAPI, Flask, Django)

Add `sentry-sdk`, `opentelemetry-distro`,
`opentelemetry-exporter-otlp-proto-http` and the framework's instrumentation
(`opentelemetry-instrumentation-fastapi`, `-flask` or `-django`) to the
requirements. Call `sentry_sdk.init(dsn=os.environ.get("SENTRY_DSN"))` before
the app is created; it does nothing without a DSN. Start the server through
`opentelemetry-instrument`, for example
`opentelemetry-instrument uvicorn main:app --host 0.0.0.0 --port $PORT`; it
reads the `OTEL_` variables itself.

## Go

Use `github.com/getsentry/sentry-go` with
`sentry.Init(sentry.ClientOptions{Dsn: os.Getenv("SENTRY_DSN")})` and
`defer sentry.Flush(2 * time.Second)`. For traces, create the exporter with
`otlptracehttp.New(ctx)`, which reads the `OTEL_` variables. Register
`sdktrace.NewTracerProvider(sdktrace.WithBatcher(exporter))` with
`otel.SetTracerProvider`, and serve through `otelhttp.NewHandler(mux, "server")`.
Only build the exporter when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

## Rules

- Plain HTML with no build step and no server has no way to read the DSN or
  the endpoint, and they must never be hardcoded, so it gets neither. Do not
  offer them for it; say it reports usage only. Adding a server or a build
  step is what would make them possible.
- Keep personal data out: leave `sendDefaultPii` off, and never put tokens,
  emails or passwords in events or span names.
- Turn on session replay only when the user asks for it.
- Check before you deploy: the build exits 0 with the SDKs in, and the app
  starts and answers in the sandbox, where none of the variables exist.
- When you finish, say whether the app reports errors and traces, and that
  they appear on the project's Errors and Traces pages once the deployment
  has traffic.
