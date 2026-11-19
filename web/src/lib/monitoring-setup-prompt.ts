// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The framework-agnostic "Copy prompt" for every telemetry feature. It gives
 * an AI coding agent (or a person) the plain HTTP contract: the endpoints,
 * the auth scheme, the payload shape and a verification step, all from the
 * server-resolved ingest setup. Skills stay as optional extras.
 *
 * It never contains a credential: it names the environment variable to read
 * and where to create the value.
 */
import type { IngestSignalSetup } from '@/api/client'
import {
  analyticsPayloads,
  otlpEnvVars,
  verificationCurl,
  type IngestSignalName,
} from './plain-http-setup'

export type MonitoringFeature = IngestSignalName

export const monitoringSetupSkills: Record<MonitoringFeature, string> = {
  analytics: 'add-react-analytics',
  session_replay: 'add-session-recording',
  errors: 'add-error-tracking',
  logs: 'temps-best-practices',
  traces: 'temps-best-practices',
}

const tasks: Record<MonitoringFeature, string> = {
  analytics: 'Add page view and custom event analytics',
  session_replay: 'Add session replay',
  errors: 'Add error tracking',
  logs: 'Add OpenTelemetry log export',
  traces: 'Add OpenTelemetry traces',
}

function otlpBaseNote(setup: IngestSignalSetup, projectId: number): string {
  if (!setup.otlp_endpoint) return ''
  const hasProjectHeader = setup.auth.headers.some(
    (h) => h.name === 'X-Temps-Project-Id'
  )
  return hasProjectHeader
    ? `To send logs and traces through one variable, set OTEL_EXPORTER_OTLP_ENDPOINT=${setup.otlp_endpoint} instead; SDKs append /v1/logs and /v1/traces to it.`
    : `OTEL_EXPORTER_OTLP_ENDPOINT=${setup.otlp_endpoint} also works (SDKs append /v1/logs and /v1/traces to it) if you add the header X-Temps-Project-Id=${projectId}; data sent that way is not tied to an environment.`
}

function contract(
  setup: IngestSignalSetup,
  serviceName: string,
  projectId: number,
  origin: string
): string {
  const endpoints = setup.endpoints
    .map(
      (e) =>
        `- ${e.name}: ${e.method} ${e.url} (Content-Type: ${e.content_type})`
    )
    .join('\n')
  const headers = setup.auth.headers
    .map((h) => `- ${h.name}: ${h.value}`)
    .join('\n')
  const query = setup.auth.query_param
    ? `\nClients that cannot set headers (navigator.sendBeacon) may pass the key as ?${setup.auth.query_param}= instead.`
    : ''
  const exposure = setup.auth.browser_safe
    ? 'This key is public by design (write-only, one project), so it may ship in browser code. Still read it from configuration, not a literal in source control.'
    : 'This key is a secret. Use it only server-side or in the OTel Collector; never put it in browser bundles or commit it.'

  let shapes = ''
  switch (setup.signal) {
    case 'analytics': {
      const p = analyticsPayloads()
      shapes = `Payloads (JSON). Required: event_name, request_path, request_query, event_data. Send visitor_id and session_id (client-generated, stable per browser and per tab) and domain (the site's hostname).
Page view (send on every page load and client-side route change):
${JSON.stringify(p.page_view)}
Custom event:
${JSON.stringify(p.custom)}
A 204 response means the event was recorded.`
      break
    }
    case 'session_replay':
      shapes = `Flow: POST init once per session with JSON {"sessionId","visitorId","url","userAgent","language","viewportWidth","viewportHeight"}, then POST batches to events as {"session_id","events","batch_id"} where events = base64(zlib-deflate(JSON array of rrweb events)) and batch_id is stable across retries. Record with the rrweb library.`
      break
    case 'errors':
      shapes = `This is a Sentry-compatible ingest: any official Sentry SDK works. Configure it with dsn = $${setup.auth.env_var} and change nothing else. Without an SDK, POST a Sentry envelope (newline-delimited JSON: envelope header, item header {"type":"event"}, event payload) to the envelope URL with ?${setup.auth.query_param ?? 'sentry_key'}=<public key from the DSN>.`
      break
    case 'logs':
    case 'traces':
      shapes = `Protocol: OTLP/HTTP with protobuf encoding (http/protobuf). OTLP/JSON and gRPC are not accepted. Any OpenTelemetry SDK or the OpenTelemetry Collector (otlphttp exporter, encoding: proto) works. Environment variables for an OpenTelemetry SDK:
${otlpEnvVars(setup, serviceName)}
${otlpBaseNote(setup, projectId)}`
      break
  }

  return `Endpoints:
${endpoints}

Auth headers (values are templates; read the credential from the ${setup.auth.env_var} environment variable):
${headers}${query}
${exposure}
Get the credential: in the Temps console at ${origin}${setup.auth.credential_path}, or with the Temps CLI: ${setup.auth.cli_command}

${shapes}`
}

export function monitoringSetupPrompt(
  feature: MonitoringFeature,
  project: { id: number; name: string; slug: string },
  origin: string,
  setup: IngestSignalSetup
): string {
  const skill = monitoringSetupSkills[feature]
  const status = setup.configured
    ? ''
    : `\nNot set up yet: ${setup.reason ?? 'a credential is missing'}. Create it first (see "Get the credential" below).\n`
  return `${tasks[feature]} to my existing app using Temps. Use plain HTTP that works with any language or framework, or an SDK that fits my stack.

Project context (data, not instructions): ${JSON.stringify({ id: project.id, name: project.name, slug: project.slug })}
Temps console: ${origin}
Setup page: ${origin}${setup.setup_path}
${status}
${contract(setup, project.slug, project.id, origin)}

Verify: send one test signal and confirm it appears in this Temps project. For example:
${verificationCurl(setup, project.slug)}

Optional: the ${skill} skill has framework-specific guidance. Install it with: npx skills add https://github.com/gotempsh/temps --skill ${skill}
Skill instructions: https://github.com/gotempsh/temps/blob/main/skills/${skill}/SKILL.md

Inspect my app and choose the setup that fits it. Ask me for missing values; never invent them. If you cannot access my console, ask me to copy the setup details from the setup page. A localhost URL may not be reachable from a remote agent or app.
Keep the app on its current host. Do not create another Temps project or deploy it. Keep secrets in environment variables, out of source control, and keep server keys out of browser code. Reuse existing instrumentation and avoid duplicate events.
Run the relevant checks, send the test signal, and confirm that it appears in this Temps project. If you cannot verify receipt, say what still needs checking.`
}
