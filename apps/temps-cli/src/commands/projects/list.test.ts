// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerProjectsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('projects list', () => {
  test('filters by project group, or to services in none', async () => {
    const routes = { 'GET /projects': { projects: [] } }
    const grouped = await runWithApi(registerProjectsCommands, ['projects', 'list', '--group', '5', '--json'], routes)
    expect(grouped.error).toBeUndefined()
    expect(grouped.requests).toEqual(['GET /projects?project_group_id=5'])
    const ungrouped = await runWithApi(registerProjectsCommands, ['projects', 'list', '--ungrouped', '--json'], routes)
    expect(ungrouped.requests).toEqual(['GET /projects?ungrouped=true'])
    const both = await runWithApi(registerProjectsCommands, ['projects', 'list', '--group', '5', '--ungrouped'], routes)
    expect(both.error?.message).toMatch(/cannot be combined/)
    expect(both.requests).toEqual([])
  })
})
