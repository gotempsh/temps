// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerErrorsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('errors events', () => {
  test('filters events by a tag', async () => {
    const run = await runWithApi(
      registerErrorsCommands,
      ['errors', 'events', '--project-id', '7', '--group-id', '9', '--tag-key', 'release', '--tag-value', 'v2', '--json'],
      { 'GET /projects/7/error-groups/9/events': { data: [], total: 0, page: 1, page_size: 20 } },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests[0]).toContain('tag_key=release')
    expect(run.requests[0]).toContain('tag_value=v2')
  })
})
