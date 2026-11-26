// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerEnvironmentsEndpoints } from './endpoints.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('environments settings', () => {
  test('sends only the settings given', async () => {
    const run = await runWithApi(
      registerEnvironmentsEndpoints,
      [
        'environments', 'settings', '3', '-p', '7', '--on-demand', '--idle-timeout-seconds', '600', '--target-nodes', '4', '--target-nodes', '5',
        '--shield-mode', 'selective', '--shield-exempt-paths', '/api/*', '--no-protected', '--health', '{"path":"/healthz"}', '--json',
      ],
      { 'PUT /projects/7/environments/3/settings': { id: 3 } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({
      on_demand: true,
      idle_timeout_seconds: 600,
      target_nodes: [4, 5],
      shield_mode: 'selective',
      shield_exempt_paths: ['/api/*'],
      protected: false,
      health: { path: '/healthz' },
    })
  })

  test('the environment password is read from stdin', async () => {
    const run = await runWithApi(
      registerEnvironmentsEndpoints,
      ['environments', 'settings', '3', '-p', '7', '--password-stdin', '--json'],
      { 'PUT /projects/7/environments/3/settings': { id: 3 } },
      { stdin: 'preview-pass' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ password: 'preview-pass' })
  })
})
