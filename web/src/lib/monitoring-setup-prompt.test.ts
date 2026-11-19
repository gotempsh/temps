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

test('an unconfigured signal says what is missing first', () => {
  const prompt = monitoringSetupPrompt(
    'errors',
    project,
    'https://console.example.com',
    signalFixture('errors', { configured: false })
  )
  expect(prompt).toContain('Not set up yet: No active DSN exists')
})
