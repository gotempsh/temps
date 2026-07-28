# OpenTelemetry Traces

Temps ingests standard OTLP/HTTP (protobuf) trace exports — any language's OpenTelemetry SDK works unmodified, there is no Temps-specific tracing SDK. Point the standard OTLP exporter env vars at Temps instead of hand-writing requests.

**Apps deployed on Temps get this for free, no configuration needed by the user**: every deployment automatically has `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME`, and `OTEL_SERVICE_VERSION` injected — at both Docker build time (`--build-arg`) and container runtime (`-e`), so this is available however the app's OTEL SDK reads it. Never hardcode these values or ask the user for a token/endpoint to hardcode; if they're missing on a running deployment, that's a platform bug, not something to work around manually. See the top-level [SKILL.md](../SKILL.md) quickstart for the exact injection mechanism and its scope (apps deployed *through* Temps only — not Temps' own console, and not apps hosted elsewhere). The manual config below is for apps sending telemetry to a self-hosted Temps instance from outside the platform.

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

Most OTLP SDKs auto-append `/v1/traces` (or `/v1/metrics`, `/v1/logs`) to `OTEL_EXPORTER_OTLP_ENDPOINT`, landing on the path above. This is exactly the value Temps auto-injects into every deployment (`{TEMPS_API_URL}/otel`, i.e. `{base_url}/api/otel` — see `temps-deployments::workflow_planner::gather_environment_variables`, `crates/temps-deployments/src/services/workflow_planner.rs:448-452`).

For `tk_` tokens (not project-scoped by default), also set:

```bash
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<tk_token>,X-Temps-Project-Id=<project_id>
```

`dt_` (deployment token) and `si_` tokens are already project/environment/deployment-scoped and don't need the project-id header.

**Always set `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` explicitly.** Several official SDKs (Node, Python, Java, .NET) default their generic OTLP exporter to gRPC — Temps only accepts OTLP/HTTP with a protobuf body, so a gRPC export will silently fail to connect rather than fall back.

## Quickstart: instrument any language

Every OpenTelemetry SDK reads the three env vars above and needs no Temps-specific code — set them, run the app, and spans land in **Observe → Traces**. These are the same setup patterns from the [add-error-tracking](../../add-error-tracking/SKILL.md) skill's platform list, applied to traces instead of Sentry.

### Node.js (and Next.js server/API routes)

Zero-code, via the OpenTelemetry auto-instrumentation agent — no source changes:

```bash
npm install --save-dev @opentelemetry/auto-instrumentations-node
node --require @opentelemetry/auto-instrumentations-node/register app.js
```

Or set `NODE_OPTIONS="--require @opentelemetry/auto-instrumentations-node/register"` in the deployment env so it applies without changing the start command.

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

Traces from browser code use the OpenTelemetry Web SDK — the same packages work regardless of frontend framework, since instrumentation is framework-agnostic:

```bash
npm install @opentelemetry/sdk-trace-web @opentelemetry/exporter-trace-otlp-http @opentelemetry/instrumentation-fetch
```

```ts
// src/otel.ts — import this first in your app entrypoint
import { WebTracerProvider } from '@opentelemetry/sdk-trace-web';
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-http';
import { BatchSpanProcessor } from '@opentelemetry/sdk-trace-base';
import { FetchInstrumentation } from '@opentelemetry/instrumentation-fetch';
import { registerInstrumentations } from '@opentelemetry/instrumentation';

const provider = new WebTracerProvider({
  spanProcessors: [
    new BatchSpanProcessor(
      new OTLPTraceExporter({
        url: 'https://<temps-host>/api/otel/v1/traces',
        headers: { Authorization: 'Bearer <dt_or_tk_token>' },
      })
    ),
  ],
});
provider.register();
registerInstrumentations({ instrumentations: [new FetchInstrumentation()] });
```

Browser exporters can't read process env vars, so the endpoint/headers must be passed explicitly in code (via a build-time env var like other bundler-injected config, not hardcoded) rather than through `OTEL_EXPORTER_OTLP_*`.

### React Native / Flutter

Mobile OpenTelemetry SDKs are less mature than the backend/browser ecosystem and package availability changes — check the [OpenTelemetry registry](https://opentelemetry.io/ecosystem/registry/) for the current state before committing to one. For most apps it's more reliable to send mobile telemetry through your own backend (which is already instrumented per above) rather than exporting OTLP directly from the device. Error tracking is the more mature signal for mobile — see [add-error-tracking](../../add-error-tracking/SKILL.md), which has dedicated React Native and Flutter setup.

## Auth

- `Authorization: Bearer <token>` (percent-encoded `Bearer%20<token>` is also accepted — some OTLP exporters encode headers that way)
- `X-Temps-Api-Key: <token>` as an alternative to the `Authorization` header
- `X-Temps-Project-Id: <id>` — required alongside `tk_` tokens, optional/ignored for `dt_`/`si_`

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
