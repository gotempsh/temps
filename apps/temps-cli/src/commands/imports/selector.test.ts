// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerImportsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const sources = { 'GET /imports/sources': [{ source: 'docker', name: 'Docker', available: true }] }

describe('migrate discover/plan selectors', () => {
  test('discover narrows the workloads it asks for', async () => {
    const run = await runWithApi(
      registerImportsCommands,
      ['migrate', 'discover', '-s', 'docker', '--name-pattern', 'shop-*', '--status-filter', 'running', '--label-filter', '{"env":"prod"}', '--limit', '5', '--json'],
      { ...sources, 'POST /imports/discover': { workloads: [] } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({
      source: 'docker',
      credentials: {},
      selector: { name_pattern: 'shop-*', status_filter: ['running'], label_filter: { env: 'prod' }, limit: 5 },
    })
    const plain = await runWithApi(registerImportsCommands, ['migrate', 'discover', '-s', 'docker', '--json'], {
      ...sources,
      'POST /imports/discover': { workloads: [] },
    })
    expect(plain.bodies[0]?.body).toEqual({ source: 'docker', credentials: {} })
  })

  test('plan sends the repository to detect the preset from', async () => {
    const run = await runWithApi(
      registerImportsCommands,
      ['migrate', 'plan', '-s', 'docker', '-w', 'c1', '--repository-id', '12'],
      {
        ...sources,
        'POST /imports/discover': { workloads: [{ id: 'c1', name: 'shop' }] },
        'POST /imports/plan': Response.json({ title: 'Bad Request', detail: 'stop here' }, { status: 400 }),
      },
    )
    expect(run.bodies.find((b) => b.request === 'POST /imports/plan')?.body).toEqual({
      source: 'docker',
      workload_id: 'c1',
      credentials: {},
      repository_id: 12,
    })
  })
})
