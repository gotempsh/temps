// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { existsSync } from 'node:fs'
import { signalFixture } from './ingest-setup.fixture'
import {
  monitoringSetupPrompt,
  monitoringSetupSkills,
} from './monitoring-setup-prompt'

const project = { id: 42, name: 'My app', slug: 'my-app' }
const features = [
  'analytics',
  'session_replay',
  'errors',
  'logs',
  'traces',
] as const

for (const feature of features) {
  test(`${feature} prompt gives the plain HTTP contract without a secret`, () => {
    const setup = signalFixture(feature)
    const prompt = monitoringSetupPrompt(
      feature,
      project,
      'http://localhost:3000',
      setup
    )

    // Endpoints and auth, from the server-resolved setup.
    for (const endpoint of setup.endpoints) {
      expect(prompt).toContain(`${endpoint.method} ${endpoint.url}`)
    }
    for (const header of setup.auth.headers) {
      expect(prompt).toContain(`${header.name}: ${header.value}`)
    }
    expect(prompt).toContain(setup.auth.env_var)
    expect(prompt).toContain(
      `console at http://localhost:3000${setup.auth.credential_path}`
    )
    expect(prompt).toContain(setup.auth.cli_command)
    expect(prompt).toContain(`http://localhost:3000${setup.setup_path}`)
    expect(prompt).toContain('Verify: send one test signal')
    expect(prompt).toContain('curl -i -X POST')

    // No credential values: only env-var templates.
    expect(prompt).not.toMatch(/\btk_[A-Za-z0-9]{8,}/)
    expect(prompt).not.toMatch(/\bpa_[0-9a-f]{16,}/)
    expect(prompt).not.toMatch(/https?:\/\/[0-9a-f]{16,}@/)

    // Existing guidance stays.
    expect(prompt).toContain('"id":42')
    expect(prompt).toContain('Do not create another Temps project or deploy it')
    expect(prompt).toContain('never invent them')
    expect(prompt).toContain('keep server keys out of browser code')
    expect(prompt).toContain('confirm that it appears')

    // Skills are an optional extra and must exist.
    const skill = monitoringSetupSkills[feature]
    expect(existsSync(`../skills/${skill}/SKILL.md`)).toBe(true)
    expect(prompt).toContain(`Optional: the ${skill} skill`)
  })
}

test('errors prompt says any Sentry SDK works', () => {
  const prompt = monitoringSetupPrompt(
    'errors',
    project,
    'https://console.example.com',
    signalFixture('errors')
  )
  expect(prompt).toContain('any official Sentry SDK works')
})

test('OTLP prompts state the protocol and any OpenTelemetry SDK or Collector', () => {
  for (const feature of ['logs', 'traces'] as const) {
    const prompt = monitoringSetupPrompt(
      feature,
      project,
      'https://console.example.com',
      signalFixture(feature)
    )
    expect(prompt).toContain('http/protobuf')
    expect(prompt).toContain('OTLP/JSON and gRPC are not accepted')
    expect(prompt).toContain('OpenTelemetry Collector')
    expect(prompt).toContain('never put it in browser bundles')
  }
})

test('session replay prompt uses the camelCase batch fields and requires privacy safeguards', () => {
  const prompt = monitoringSetupPrompt(
    'session_replay',
    project,
    'https://console.example.com',
    signalFixture('session_replay')
  )
  // SessionReplayEventsRequest is camelCase: session_id would be rejected.
  expect(prompt).toContain('{"sessionId","events","batchId"}')
  expect(prompt).not.toContain('"session_id"')
  expect(prompt).toContain('Privacy (required)')
  expect(prompt).toContain('consents')
  expect(prompt).toContain('maskAllInputs: true')
  expect(prompt).toContain('rr-block')
})

test('an environment-scoped OTLP prompt never suggests the generic endpoint', () => {
  const projectScoped = signalFixture('traces')
  // With an environment the server sends no base and path-based URLs.
  const environmentScoped = {
    ...projectScoped,
    endpoints: [
      {
        ...projectScoped.endpoints[0],
        url: 'https://temps.example.com/api/otel/v1/42/7/0/traces',
      },
    ],
    auth: {
      ...projectScoped.auth,
      headers: projectScoped.auth.headers.filter(
        (h) => h.name !== 'X-Temps-Project-Id'
      ),
    },
    otlp_endpoint: null,
  }
  const prompt = monitoringSetupPrompt(
    'traces',
    project,
    'https://console.example.com',
    environmentScoped
  )
  expect(prompt).toContain('not OTEL_EXPORTER_OTLP_ENDPOINT')
  expect(prompt).not.toContain('OTEL_EXPORTER_OTLP_ENDPOINT=')
  expect(prompt).toContain(
    'OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=https://temps.example.com/api/otel/v1/42/7/0/traces'
  )

  // Without an environment the base and its project header are offered.
  const generic = monitoringSetupPrompt(
    'traces',
    project,
    'https://console.example.com',
    projectScoped
  )
  expect(generic).toContain(
    'OTEL_EXPORTER_OTLP_ENDPOINT=https://temps.example.com/api/otel'
  )
})

test('analytics and replay prompts explain allowed origins outside a browser', () => {
  for (const feature of ['analytics', 'session_replay'] as const) {
    const prompt = monitoringSetupPrompt(
      feature,
      project,
      'https://console.example.com',
      signalFixture(feature)
    )
    expect(prompt).toContain(
      'must send an Origin header matching one of them, or they get 403'
    )
  }
})

test('an unconfigured signal says what is missing first', () => {
  const prompt = monitoringSetupPrompt(
    'errors',
    project,
    'https://console.example.com',
    signalFixture('errors', { configured: false })
  )
  expect(prompt).toContain('Not set up yet: No active DSN exists')
})
