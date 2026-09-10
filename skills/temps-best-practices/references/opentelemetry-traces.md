# OpenTelemetry Traces

Temps ingests standard OTLP/HTTP (protobuf) trace exports. There is no Temps-specific tracing SDK: install and initialize the official OpenTelemetry SDK for the application's language, then point its standard exporter variables at Temps.

Apps deployed on Temps receive `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME`, and `OTEL_SERVICE_VERSION`. This configures export only—it does not install instrumentation, choose production sampling, filter health traffic, propagate context, or flush on shutdown. `OTEL_EXPORTER_OTLP_HEADERS` contains a live deployment token and is server-only.

## Contents

- [Endpoints](#endpoints)
- [Standard OTLP exporter config](#standard-otlp-exporter-config)
- [Required: exclude the health-check path](#required-exclude-the-health-check-path)
- [Production baseline](#production-baseline)
- [Quickstart: instrument any language](#quickstart-instrument-any-language)
- [Auth](#auth)
- [Rate limits and quota](#rate-limits-and-quota)
- [Storage](#storage)
- [Critical gotcha: trace duration units](#critical-gotcha-trace-duration-units)
- [Gotchas](#gotchas)

## Endpoints

Every plugin's routes (public and authenticated alike) are nested under `/api` by the console server (`temps-cli/src/commands/serve/console.rs`), so the real externally-reachable paths are:

- Header-based: `POST /api/otel/v1/traces` — project/environment/deployment resolved from the auth token
- Path-based: `POST /api/otel/v1/{project_id}/{environment_id}/{deployment_id}/traces` — explicit scoping in the URL

Both accept `application/x-protobuf` bodies (`ExportTraceServiceRequest`), with optional gzip or zstd `Content-Encoding`.

Implemented in `temps-otel` crate: `src/handlers/mod.rs` (route table, doc comment lists the full path set), `src/handlers/ingest_handler.rs` (handler logic).

## Standard OTLP exporter config

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=https://<temps-host>/api/otel
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<dt_or_tk_token>
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

Most OTLP SDKs auto-append `/v1/traces` (or `/v1/metrics`, `/v1/logs`) to `OTEL_EXPORTER_OTLP_ENDPOINT`, landing on the path above.

For `tk_` tokens (not project-scoped by default), also set:

```bash
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<tk_token>,X-Temps-Project-Id=<project_id>
```

`dt_` deployment tokens don't need the project-id header. `si_` integration tokens are for infrastructure metrics ingestion; do not use them for application traces or logs.

**Always set `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` explicitly.** Several official SDKs (Node, Python, Java, .NET) default their generic OTLP exporter to gRPC — Temps only accepts OTLP/HTTP with a protobuf body, so a gRPC export will silently fail to connect rather than fall back.

## Required: exclude the health-check path

OpenTelemetry server tracing is not complete until the path configured in the effective app root's `.temps.yaml` is excluded from incoming-request instrumentation:

```yaml
health:
  path: /healthz
```

Filter the exact URL pathname before its server span is created or exported. Do not assume the generic `OTEL_EXPORTER_OTLP_*` variables do this: they configure export, not request filtering, and OTel does not define one cross-language health-path exclusion variable. Use the current request-ignore mechanism provided by the app's language/framework instrumentation.

For Node.js auto-instrumentation, use the HTTP instrumentation's `ignoreIncomingRequestHook`. The zero-code `@opentelemetry/auto-instrumentations-node/register` preload cannot accept programmatic instrumentation options, so use a small preload file when health filtering is required. Add dependencies through the application's reviewed dependency workflow, commit the lockfile, and use exact versions; disable lifecycle scripts where compatible:

```bash
npm install --ignore-scripts --save-exact @opentelemetry/sdk-node \
  @opentelemetry/exporter-trace-otlp-proto \
  @opentelemetry/auto-instrumentations-node
```

```js
// tracing.cjs — load before the application
const { NodeSDK } = require('@opentelemetry/sdk-node');
const {
  OTLPTraceExporter,
} = require('@opentelemetry/exporter-trace-otlp-proto');
const {
  getNodeAutoInstrumentations,
} = require('@opentelemetry/auto-instrumentations-node');

const HEALTH_PATH = '/healthz'; // must match .temps.yaml health.path

const sdk = new NodeSDK({
  traceExporter: new OTLPTraceExporter(), // reads OTEL_EXPORTER_OTLP_* env vars
  instrumentations: [
    getNodeAutoInstrumentations({
      '@opentelemetry/instrumentation-http': {
        ignoreIncomingRequestHook(request) {
          const pathname = new URL(
            request.url || '/',
            'http://localhost',
          ).pathname;
          return pathname === HEALTH_PATH;
        },
      },
    }),
  ],
});

sdk.start();

process.once('SIGTERM', async () => {
  await sdk.shutdown(); // compose with the app's request/DB shutdown path
});
```

Start with `NODE_OPTIONS="--require ./tracing.cjs"` or `node --require ./tracing.cjs app.js`.

Framework wrappers may expose different filters. Confirm that the option applies to **incoming server requests**; an outbound `fetch` ignore list does not suppress the server span created when Temps calls the app. If the wrapper cannot filter incoming requests, use its supported sampler/span-processor approach or a configurable SDK bootstrap, and verify the result rather than assuming the route is ignored.

Verification is behavioral: call the health route several times and confirm no corresponding server spans appear in **Observe → Traces**, then call a normal route and confirm that its span still appears. If either side fails, the exclusion is too narrow or too broad.

The scale-to-zero wake probe currently calls `/` independently of `.temps.yaml`. Do not exclude `/` to hide those spans because that would also hide legitimate root-route traffic. See [runtime-contract.md](runtime-contract.md).

## Production baseline

Apply [telemetry-hygiene.md](telemetry-hygiene.md) before enabling production export:

- Use a batch span processor with a bounded queue and export timeout.
- Choose an environment-appropriate parent-based trace-id-ratio sampler rather than assuming 100% collection is affordable:

  ```bash
  OTEL_TRACES_SAMPLER=parentbased_traceidratio
  OTEL_TRACES_SAMPLER_ARG=0.05
  ```

  `0.05` is an example. Base the real ratio on traffic volume and debugging needs.
- Exclude health checks, static assets, and low-value middleware/socket spans at the source.
- Use route-template span names and drop high-cardinality attributes.
- Extract inbound and inject outbound W3C `traceparent`/`tracestate`. Temps configures export; application instrumentation remains responsible for propagation.
- Preserve parent sampling decisions across services.
- Flush/shut down the provider during `SIGTERM` inside Temps' 10-second container stop window.
- Never put raw tokens, authorization/cookie headers, request bodies, sensitive query values, or unredacted GenAI prompts/completions in spans.

## Quickstart: instrument any language

Every OpenTelemetry SDK reads the three env vars above and needs no Temps-specific code — set them, run the app, and spans land in **Observe → Traces**. These are the same setup patterns from the [add-error-tracking](../../add-error-tracking/SKILL.md) skill's platform list, applied to traces instead of Sentry.

### Node.js (and Next.js server/API routes)

For a quick local smoke test, the zero-code OpenTelemetry auto-instrumentation agent needs no source changes:

```bash
npm install --save-dev @opentelemetry/auto-instrumentations-node
node --require @opentelemetry/auto-instrumentations-node/register app.js
```

Or set `NODE_OPTIONS="--require @opentelemetry/auto-instrumentations-node/register"` in the deployment env so it applies without changing the start command.

For a Temps deployment, replace this zero-code preload with the configurable `tracing.cjs` bootstrap above so the `.temps.yaml` health path is excluded.

For Next.js specifically, use the built-in `instrumentation.ts` hook instead:

```bash
npm install @vercel/otel
```

```ts
// instrumentation.ts (project root)
import { registerOTel } from '@vercel/otel';

export function register() {
  registerOTel({ serviceName: 'your-app' });
}
```

`@vercel/otel` reads the same `OTEL_EXPORTER_OTLP_*` env vars — no Temps-specific config needed.

`instrumentationConfig.fetch.ignoreUrls` controls outbound fetch instrumentation; it does not prove the incoming Next.js health request is suppressed. Use the wrapper/version's supported incoming-span filter or a custom processor/bootstrap and verify behavior in Temps. Do not mark the integration complete merely because `/healthz` appears in an outbound ignore list.

**Critical gotcha if the app also uses `@sentry/nextjs`**: Sentry's Next.js SDK registers its own global `TracerProvider`/`ContextManager` by default. If `Sentry.init()` runs before `registerOTel()` — which it normally does, since Sentry's config files load ahead of `instrumentation.ts`'s `register()` — every span becomes non-recording and **traces silently stop reaching Temps with no error anywhere**. Fix by passing `skipOpenTelemetrySetup: true` to `Sentry.init()` in `sentry.server.config.ts` (and `sentry.edge.config.ts` if used) so `@vercel/otel` remains the sole tracer provider:

```ts
// sentry.server.config.ts
Sentry.init({
  dsn: process.env.SENTRY_DSN,
  skipOpenTelemetrySetup: true, // let @vercel/otel own tracing — see gotcha above
  tracesSampleRate: 1.0,
});
```

This is a real, previously-hit bug, not a hypothetical: a Temps example app went through several iterations before landing on this fix (`observability-starter` in `temps-examples`, commits fixing "let @vercel/otel own tracing so traces reach Temps"). If error tracking and traces are both being wired up on the same Next.js app, apply this from the start.

### Python

Zero-code via `opentelemetry-instrument`:

```bash
pip install opentelemetry-distro opentelemetry-exporter-otlp
opentelemetry-bootstrap -a install
opentelemetry-instrument --service_name your-app python app.py
```

### Go

Go has no stable zero-code agent — initialize the SDK manually and read the standard env vars via `autoexport`:

```bash
go get go.opentelemetry.io/otel go.opentelemetry.io/contrib/exporters/autoexport go.opentelemetry.io/otel/sdk
```

```go
package main

import (
	"context"

	"go.opentelemetry.io/contrib/exporters/autoexport"
	"go.opentelemetry.io/otel"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
)

func main() {
	ctx := context.Background()
	exporter, err := autoexport.NewSpanExporter(ctx) // reads OTEL_EXPORTER_OTLP_* env vars
	if err != nil {
		panic(err)
	}
	tp := sdktrace.NewTracerProvider(sdktrace.WithBatcher(exporter))
	otel.SetTracerProvider(tp)
	defer tp.Shutdown(ctx)

	// your app
}
```

### Rust

```bash
cargo add opentelemetry opentelemetry-otlp opentelemetry_sdk --features opentelemetry-otlp/http-proto,opentelemetry-otlp/reqwest-client
```

```rust
use opentelemetry_otlp::WithExportConfig;

fn main() {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http() // reads OTEL_EXPORTER_OTLP_* env vars
        .build()
        .expect("failed to build OTLP exporter");

    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .build();

    opentelemetry::global::set_tracer_provider(provider);

    // your app
}
```

### Ruby

Zero-code via the instrumentation-all gem:

```bash
bundle add opentelemetry-sdk opentelemetry-exporter-otlp opentelemetry-instrumentation-all
```

```ruby
# config/initializers/otel.rb (or app entrypoint)
require 'opentelemetry/sdk'
require 'opentelemetry/exporter/otlp'
require 'opentelemetry/instrumentation/all'

OpenTelemetry::SDK.configure do |c|
  c.service_name = 'your-app'
  c.use_all # reads OTEL_EXPORTER_OTLP_* env vars
end
```

### Java

Zero-code via the OpenTelemetry Java agent — no source changes:

```bash
curl -L -o opentelemetry-javaagent.jar \
  https://github.com/open-telemetry/opentelemetry-java-instrumentation/releases/latest/download/opentelemetry-javaagent.jar
java -javaagent:opentelemetry-javaagent.jar -Dotel.service.name=your-app -jar app.jar
```

The agent reads the same `OTEL_EXPORTER_OTLP_*` env vars.

### PHP

```bash
composer require open-telemetry/opentelemetry open-telemetry/exporter-otlp open-telemetry/transport-grpc
```

```php
<?php
// bootstrap.php — include before your app's entrypoint
use OpenTelemetry\API\Globals;
use OpenTelemetry\SDK\Trace\TracerProviderFactory;

$tracerProvider = (new TracerProviderFactory())->create(); // reads OTEL_EXPORTER_OTLP_* env vars
```

### .NET

```bash
dotnet add package OpenTelemetry.Extensions.Hosting
dotnet add package OpenTelemetry.Exporter.OpenTelemetryProtocol
```

```csharp
// Program.cs
builder.Services.AddOpenTelemetry()
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddOtlpExporter()); // reads OTEL_EXPORTER_OTLP_* env vars
```

### Browser (React, Vue, Svelte, Angular)

Do not export browser traces directly to Temps with a `dt_`/`tk_` bearer token. Those are live server credentials, and putting one in JavaScript or a public build-time variable exposes it to every visitor. Temps' OTLP routes are not a public browser-ingest surface.

If browser tracing is required:

1. Collect with the OpenTelemetry Web SDK.
2. Send to a same-origin application backend or trusted collector.
3. Validate and rate-limit the public request there.
4. Add the Temps OTLP credential only on the server-side hop.

For most browser applications, use Temps analytics/Web Vitals plus Sentry-compatible error tracking and keep full OTLP export on the server. See [telemetry-hygiene.md](telemetry-hygiene.md).

### React Native / Flutter

Never embed a Temps `dt_`/`tk_` token in a mobile binary. Send mobile telemetry through an authenticated application backend/collector. Error tracking is the more mature direct-device signal — see [add-error-tracking](../../add-error-tracking/SKILL.md).

## Auth

- `Authorization: Bearer <token>` (Temps also accepts literal `Bearer%20<token>` produced by some exporters)
- `X-Temps-Api-Key: <token>` as an alternative
- `X-Temps-Project-Id: <id>` — required with `tk_`; unnecessary with `dt_`
- `si_` — infrastructure metrics only, not application traces/logs

Prefer the header-based endpoint with a `dt_` so Temps resolves project/environment/deployment attribution from authentication. Do not expose machine tokens to untrusted clients.

## Rate limits and quota

- 1000 requests / 60s per token by default (`TEMPS_OTEL_RATE_LIMIT`, `TEMPS_OTEL_RATE_LIMIT_WINDOW_SECS`), enforced before protobuf parsing — 429 on exceed.
- Storage quota is opt-in and off by default (`TEMPS_OTEL_QUOTA_GB`); when enabled, over-quota ingestion returns 413.
- Decompression bomb protection caps decompressed payload size; oversized payloads return 400.

## Storage

- Spans land in a hypertable (TimescaleDB) segmented by `trace_id`; ClickHouse is used for some analytics reads (see project history: `otel_spans compression fix PR #348`).
- Always query traces with a time bound — the hypertable lookups are unbounded by default if you don't pass one, which is expensive at scale.

## Critical gotcha: trace duration units

`duration_ms` on a `SpanRecord` is the **only** field guaranteed to be milliseconds. Everything else in a span's `attributes` map carries whatever unit the instrumenting library reported:

- `gen_ai.*` semantic-convention attributes (LLM call attributes) are commonly in **seconds**, not ms.
- Other attributes may be nanoseconds or microseconds depending on the source library.

This bit the AI trace-summarization feature (see `crates/temps-otel/src/handlers/query_handler.rs`, `crates/temps-otel/src/types.rs`) — it treated raw `gen_ai.*` attribute values as milliseconds and produced wildly inflated duration claims. When computing or displaying a duration from a span, always use `duration_ms` directly rather than reading a raw attribute unless you've confirmed that attribute's unit from the emitting SDK's semantic conventions.

## Gotchas

- **401s**: check token prefix matches what the endpoint expects and the header is well-formed (`Bearer <token>`, not just the raw token).
- **429s under load testing**: expected past 1000 req/60s per token — use multiple tokens or reduce export frequency (batch spans) rather than treating it as a bug.
- **Spans not appearing**: confirm `Content-Type: application/x-protobuf` — Temps does not accept OTLP/JSON on these endpoints.
