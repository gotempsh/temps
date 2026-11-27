// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerProvidersEndpoints } from './endpoints.js'
import { runWithApi } from '../../lib/mock-api.js'

const branch = { name: 'release/2026-01', commit_sha: 'abc123', protected: false, is_default: false }

describe('exact single-branch lookup', () => {
  test('repos get-repository-branch asks the connection for exactly the named branch', async () => {
    const run = await runWithApi(
      registerProvidersEndpoints,
      ['providers', 'repos', 'get-repository-branch', 'temps', 'example', '--connection-id', '4', '--name', 'release/2026-01', '--json'],
      { 'GET /repositories/temps/example/branch': branch },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /repositories/temps/example/branch?connection_id=4&name=release%2F2026-01'])
    expect(JSON.parse(run.stdout)).toMatchObject({ name: 'release/2026-01', is_default: false })
  })

  test('public-repos get-branch looks the branch up on the public provider', async () => {
    const run = await runWithApi(
      registerProvidersEndpoints,
      ['providers', 'public-repos', 'get-branch', 'github', 'temps', 'example', '--name', 'main', '--json'],
      { 'GET /git/public/github/temps/example/branch': { ...branch, name: 'main', is_default: true } },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /git/public/github/temps/example/branch?name=main'])
    expect(JSON.parse(run.stdout)).toMatchObject({ name: 'main', is_default: true })
  })

  test('a branch the provider does not have fails the command instead of printing a result', async () => {
    const run = await runWithApi(
      registerProvidersEndpoints,
      ['providers', 'repos', 'get-repository-branch', 'temps', 'example', '--connection-id', '4', '--name', 'gone', '--json'],
      {
        'GET /repositories/temps/example/branch': Response.json(
          { title: 'Branch not found', detail: "Branch 'gone' not found in temps/example", status: 404 },
          { status: 404 },
        ),
      },
    )
    expect(run.error).toBeDefined()
    expect(run.stdout).not.toContain('"name"')
  })

  test('the branch name is required, so nothing is sent without it', async () => {
    const run = await runWithApi(
      registerProvidersEndpoints,
      ['providers', 'public-repos', 'get-branch', 'github', 'temps', 'example', '--json'],
      {},
    )
    expect(run.error).toBeDefined()
    expect(run.requests).toEqual([])
  })
})
