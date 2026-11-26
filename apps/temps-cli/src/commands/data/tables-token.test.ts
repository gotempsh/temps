// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerDataCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('data tables', () => {
  test('--next-token continues from a previous page', async () => {
    const run = await runWithApi(
      registerDataCommands,
      ['data', 'tables', '4', '--path', 'app/public', '--limit', '10', '--next-token', 'next-page-token', '--json'],
      {
        'GET /external-services': [{ id: 4, name: 'db', service_type: 'postgres' }],
        'GET /external-services/4/query/containers/app%2Fpublic/entities': { entities: [] },
      },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toContain('GET /external-services/4/query/containers/app%2Fpublic/entities?limit=10&token=next-page-token')
  })
})
