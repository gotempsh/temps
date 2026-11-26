// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { Command } from 'commander'
import { registerCloudCommands } from './index.js'
import { registerCloudEndpoints } from './endpoints.js'
import { runWithApi } from '../../lib/mock-api.js'

const register = (program: Command) => {
  registerCloudCommands(program)
  registerCloudEndpoints(program)
}

describe('cloud features', () => {
  test('sends only the switches given', async () => {
    const run = await runWithApi(register, ['cloud', 'features', '--backups-enabled', '--no-telemetry-enabled', '--json'], {
      'PATCH /cloud/features': { connected: true },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([{ request: 'PATCH /cloud/features', body: { backups_enabled: true, telemetry_enabled: false } }])
  })
})

describe('cloud telemetry write-mode set --analytics', () => {
  const routes = {
    'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' },
    'PATCH /otel/cloud-telemetry/projects/7': { write_mode: 'local', analytics_write_mode: 'cloud' },
  }

  test('sends the analytics write mode with the span write mode', async () => {
    const run = await runWithApi(register, ['cloud', 'telemetry', 'write-mode', 'set', 'local', '-p', 'web', '--analytics', 'cloud', '--yes', '--json'], routes)
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ write_mode: 'local', analytics_write_mode: 'cloud' })
  })

  test('moving analytics to Cloud asks first; an unknown mode fails before any request', async () => {
    const refused = await runWithApi(register, ['cloud', 'telemetry', 'write-mode', 'set', 'local', '-p', 'web', '--analytics', 'cloud'], routes, { stdin: '' })
    expect(refused.requests.filter((r) => r.startsWith('PATCH'))).toEqual([])
    const bad = await runWithApi(register, ['cloud', 'telemetry', 'write-mode', 'set', 'local', '-p', 'web', '--analytics', 'remote'], routes)
    expect(bad.error?.message).toMatch(/Unknown analytics write mode "remote"/)
    expect(bad.requests).toEqual([])
  })
})
