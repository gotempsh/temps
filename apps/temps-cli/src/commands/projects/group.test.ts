// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { runWithApi } from '../../lib/mock-api.js'
import { registerProjectsCommands } from './index.js'

const web = {
  id: 4,
  name: 'web',
  slug: 'web',
  directory: './',
  main_branch: 'main',
  created_at: 1_767_225_600,
  updated_at: 1_767_225_600,
}
const shop = {
  id: 9,
  name: 'Shop',
  slug: 'shop',
  created_at: '2026-01-01T00:00:00Z',
  projects: [
    { id: 4, name: 'web', slug: 'web' },
    { id: 5, name: 'api', slug: 'api' },
  ],
}

describe('projects group', () => {
  test("shows the service's project (group)", async () => {
    const run = await runWithApi(registerProjectsCommands, ['projects', 'group', '-p', 'web', '--json'], {
      'GET /projects/by-slug/web': web,
      'GET /projects/4/group': { group: shop },
    })
    expect(run.error).toBeUndefined()
    expect(JSON.parse(run.stdout)).toEqual({ service: 'web', group: shop })
  })

  test('a standalone service says how to add it to a project', async () => {
    const run = await runWithApi(registerProjectsCommands, ['projects', 'group', '-p', 'web'], {
      'GET /projects/by-slug/web': web,
      'GET /projects/4/group': { group: null },
    })
    expect(run.error).toBeUndefined()
    expect(run.stdout).toContain('standalone')
    expect(run.stdout).toContain('project-groups add <project> web')
  })
})

describe('projects show', () => {
  test('includes the project (group) the service is in', async () => {
    const run = await runWithApi(registerProjectsCommands, ['projects', 'show', '-p', 'web', '--json'], {
      'GET /projects/by-slug/web': web,
      'GET /projects/4/group': { group: shop },
    })
    expect(run.error).toBeUndefined()
    expect(JSON.parse(run.stdout).project_group).toEqual(shop)
  })

  test('still shows the service when its project cannot be loaded, and says so', async () => {
    const run = await runWithApi(registerProjectsCommands, ['projects', 'show', '-p', 'web'], {
      'GET /projects/by-slug/web': web,
    })
    expect(run.error).toBeUndefined()
    expect(run.stdout).toContain('unknown (')
  })
})

describe('projects create --external', () => {
  test('--yes without --name fails with a non-zero exit before any request', async () => {
    const run = await runWithApi(registerProjectsCommands, ['projects', 'create', '--external', '--yes'], {})
    expect(run.exitCode).toBe(1)
    expect(run.stderr).toContain('--name')
    expect(run.requests).toEqual([])
  })
})
