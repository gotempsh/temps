// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Test fixture: the shape `GET /projects/{id}/ingest-setup` returns for a
 * project `my-app` (id 42) on `https://temps.example.com`, mirroring
 * `crates/temps-projects/src/services/ingest_setup.rs`. Imported by tests only.
 */
import type { IngestSignalSetup, ProjectIngestSetup } from '@/api/client'

const base = 'https://temps.example.com'

export function ingestSetupFixture({
  configured = true,
}: { configured?: boolean } = {}): ProjectIngestSetup {
  const analyticsAuth = {
    kind: 'analytics_ingest_key' as const,
    env_var: 'TEMPS_ANALYTICS_KEY',
    headers: [{ name: 'X-Temps-Analytics-Key', value: '$TEMPS_ANALYTICS_KEY' }],
    query_param: 'temps_key',
    browser_safe: true,
    credential_path: '/projects/my-app/analytics/setup',
    cli_command: 'temps analytics keys create --project my-app',
  }
  const apiKeyAuth = {
    kind: 'api_key' as const,
    env_var: 'TEMPS_API_KEY',
    headers: [
      { name: 'Authorization', value: 'Bearer $TEMPS_API_KEY' },
      { name: 'X-Temps-Project-Id', value: '42' },
    ],
    query_param: null,
    browser_safe: false,
    credential_path: '/settings/keys/new?name=my-app-telemetry',
    cli_command:
      'temps apikeys create --name my-app-telemetry --role custom --permissions otel:write --yes',
  }
  const analyticsReason = configured
    ? null
    : 'No active analytics ingest key exists for this project yet'
  const signals: IngestSignalSetup[] = [
    {
      signal: 'analytics',
      endpoints: [
        {
          name: 'event',
          method: 'POST',
          url: `${base}/api/_temps/event`,
          content_type: 'application/json',
        },
      ],
      auth: analyticsAuth,
      otlp_endpoint: null,
      configured,
      reason: analyticsReason,
      setup_path: '/projects/my-app/analytics/setup',
    },
    {
      signal: 'session_replay',
      endpoints: [
        {
          name: 'init',
          method: 'POST',
          url: `${base}/api/_temps/session-replay/init`,
          content_type: 'application/json',
        },
        {
          name: 'events',
          method: 'POST',
          url: `${base}/api/_temps/session-replay/events`,
          content_type: 'application/json',
        },
      ],
      auth: analyticsAuth,
      otlp_endpoint: null,
      configured,
      reason: analyticsReason,
      setup_path: '/projects/my-app/analytics/setup',
    },
    {
      signal: 'errors',
      endpoints: [
        {
          name: 'envelope',
          method: 'POST',
          url: `${base}/api/42/envelope/`,
          content_type: 'application/x-sentry-envelope',
        },
        {
          name: 'store',
          method: 'POST',
          url: `${base}/api/42/store/`,
          content_type: 'application/json',
        },
      ],
      auth: {
        kind: 'sentry_dsn',
        env_var: 'SENTRY_DSN',
        headers: [
          {
            name: 'X-Sentry-Auth',
            value:
              'Sentry sentry_version=7, sentry_key=<public key from $SENTRY_DSN>',
          },
        ],
        query_param: 'sentry_key',
        browser_safe: true,
        credential_path: '/projects/my-app/errors/setup',
        cli_command: 'temps dsn get-or-create --project-id 42',
      },
      otlp_endpoint: null,
      configured,
      reason: configured ? null : 'No active DSN exists for this project yet',
      setup_path: '/projects/my-app/errors/setup',
    },
    ...(['logs', 'traces'] as const).map((signal) => ({
      signal,
      endpoints: [
        {
          name: `otlp_${signal}`,
          method: 'POST',
          url: `${base}/api/otel/v1/${signal}`,
          content_type: 'application/x-protobuf',
        },
      ],
      auth: apiKeyAuth,
      otlp_endpoint: `${base}/api/otel`,
      configured: true,
      reason: null,
      setup_path:
        signal === 'logs'
          ? '/projects/my-app/telemetry-logs'
          : '/projects/my-app/traces#traces-setup',
    })),
  ]
  return {
    project_id: 42,
    project_slug: 'my-app',
    environment_id: null,
    public_base_url: base,
    signals,
  }
}

export function signalFixture(
  signal: IngestSignalSetup['signal'],
  options?: { configured?: boolean }
): IngestSignalSetup {
  const found = ingestSetupFixture(options).signals.find(
    (s) => s.signal === signal
  )
  if (!found) throw new Error(`no ${signal} in fixture`)
  return found
}
