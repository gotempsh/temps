// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerGlobalLogsEndpoints } from './endpoints.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('logs project-search', () => {
  test('sends every filter in the body', async () => {
    const run = await runWithApi(
      registerGlobalLogsEndpoints,
      [
        'logs', 'project-search', '--project-id', '7', '--text', 'timeout', '--levels', 'ERROR', '--envs', 'production',
        '--container-ids', 'c1', '--node-ids', '2', '--deploy-id', '40', '--context-lines', '3', '--cursor', 'next', '--json',
      ],
      { 'POST /logs/search': { lines: [], next_cursor: null } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({
      project_id: 7,
      text: 'timeout',
      levels: ['ERROR'],
      envs: ['production'],
      container_ids: ['c1'],
      node_ids: [2],
      deploy_id: 40,
      context_lines: 3,
      cursor: 'next',
    })
  })
})
