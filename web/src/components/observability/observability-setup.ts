// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SetupSnippet } from '@/components/onboarding/SetupGuide'

/**
 * What the empty analytics, errors, traces and logs pages teach. Values that
 * differ per service (an ingest key, a DSN for an app not on Temps) stay
 * placeholders: each service's own setup page fills them in. Apps deployed on
 * Temps receive `SENTRY_DSN` and `OTEL_EXPORTER_OTLP_*` automatically
 * (`temps-deployments` env resolver).
 */
export type ObservabilityKind = 'analytics' | 'errors' | 'traces' | 'logs'

/** Where a service sets the feature up, relative to `/projects/{slug}`. */
export const serviceSetupPath: Record<ObservabilityKind, string> = {
  analytics: 'analytics/setup',
  errors: 'errors/setup',
  traces: 'traces',
  logs: 'logs',
}

export function serviceSetupHref(
  kind: ObservabilityKind,
  projectSlug?: string
): string {
  return projectSlug
    ? `/projects/${encodeURIComponent(projectSlug)}/${serviceSetupPath[kind]}`
    : '/projects'
}

export const analyticsSnippets: SetupSnippet[] = [
  {
    value: 'react',
    label: 'React',
    install: 'npm install @temps-sdk/react-analytics',
    language: 'tsx',
    code: `import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics'

<TempsAnalyticsProvider basePath="<base-path>" ingestKey="<ingest-key>">
  {children}
</TempsAnalyticsProvider>`,
  },
  {
    value: 'script',
    label: 'Any website',
    language: 'html',
    code: `<script
  defer
  data-base-path="<base-path>"
  data-ingest-key="<ingest-key>"
  src="https://cdn.jsdelivr.net/npm/@temps-sdk/analytics-browser@latest/dist/temps.min.js">
</script>`,
  },
]

export const errorSnippets: SetupSnippet[] = [
  {
    value: 'node',
    label: 'Node.js',
    install: 'npm install @sentry/node',
    language: 'typescript',
    code: `import * as Sentry from '@sentry/node'

Sentry.init({ dsn: process.env.SENTRY_DSN })`,
  },
  {
    value: 'nextjs',
    label: 'Next.js',
    install: 'npm install @sentry/nextjs',
    language: 'typescript',
    code: `// instrumentation.ts
import * as Sentry from '@sentry/nextjs'

export function register() {
  Sentry.init({ dsn: process.env.SENTRY_DSN })
}`,
  },
  {
    value: 'python',
    label: 'Python',
    install: 'pip install sentry-sdk',
    language: 'python',
    code: `import os
import sentry_sdk

sentry_sdk.init(dsn=os.environ["SENTRY_DSN"])`,
  },
]

export const testErrorSnippet = `Sentry.captureException(new Error('Test error from my app'))`

export const traceSnippets: SetupSnippet[] = [
  {
    value: 'node',
    label: 'Node.js',
    install: `npm install @opentelemetry/sdk-node @opentelemetry/api \\
  @opentelemetry/auto-instrumentations-node \\
  @opentelemetry/exporter-trace-otlp-proto`,
    language: 'typescript',
    code: `// tracing.ts: import this first, before your app code.
import { NodeSDK } from '@opentelemetry/sdk-node'
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto'
import { getNodeAutoInstrumentations } from '@opentelemetry/auto-instrumentations-node'

new NodeSDK({
  serviceName: 'my-service',
  traceExporter: new OTLPTraceExporter(),
  instrumentations: [getNodeAutoInstrumentations()],
}).start()`,
  },
  {
    value: 'nextjs',
    label: 'Next.js',
    install: 'npm install @vercel/otel @opentelemetry/api',
    language: 'typescript',
    code: `// instrumentation.ts (project root)
import { registerOTel } from '@vercel/otel'

export function register() {
  registerOTel({ serviceName: 'my-service' })
}`,
  },
  {
    value: 'python',
    label: 'Python',
    install: `pip install opentelemetry-distro opentelemetry-exporter-otlp
opentelemetry-bootstrap --action=install`,
    language: 'bash',
    code: `OTEL_SERVICE_NAME=my-service \\
OTEL_TRACES_EXPORTER=otlp \\
opentelemetry-instrument python main.py`,
  },
]

/** The OTLP settings an app outside Temps sets by hand. */
export const traceEnvironment = `OTEL_EXPORTER_OTLP_ENDPOINT=https://<your-temps-domain>/api/otel
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <api-key>`

export const logSnippet = `console.log('order placed', { orderId: 42 })
console.error('payment failed', error)`
