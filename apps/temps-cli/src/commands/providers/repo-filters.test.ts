// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerProvidersCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('providers git repos filters', () => {
  test('the synced list filters by preset, connection and update time', async () => {
    const run = await runWithApi(
      registerProvidersCommands,
      ['providers', 'git', 'repos', '--preset', 'nextjs', '--git-provider-connection-id', '3', '--updated-after', '2026-01-01T00:00:00Z', '--json'],
      { 'GET /repositories': { repositories: [], total_count: 0 } },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests[0]).toContain('preset=nextjs')
    expect(run.requests[0]).toContain('git_provider_connection_id=3')
    expect(run.requests[0]).toContain('updated_after=2026-01-01T00%3A00%3A00Z')
  })

  test('one connection takes --updated-after but not the synced-list filters', async () => {
    const ok = await runWithApi(
      registerProvidersCommands,
      ['providers', 'git', 'repos', '--id', '3', '--updated-after', '2026-01-01T00:00:00Z', '--json'],
      { 'GET /git-connections/3/repositories': { repositories: [], total_count: 0 } },
    )
    expect(ok.error).toBeUndefined()
    expect(ok.requests[0]).toStartWith('GET /git-connections/3/repositories?')
    expect(ok.requests[0]).toContain('updated_after=')
    const bad = await runWithApi(registerProvidersCommands, ['providers', 'git', 'repos', '--id', '3', '--preset', 'nextjs'], {})
    expect(bad.error?.message).toMatch(/cannot be used with --id/)
    expect(bad.requests).toEqual([])
  })

  test('update-token sends a refresh token read from stdin', async () => {
    const run = await runWithApi(
      registerProvidersCommands,
      ['providers', 'connections', 'update-token', '--id', '3', '--token', 'access-value', '--refresh-token-stdin'],
      { 'POST /git-connections/3/update-token': { connection_id: 3, is_active: true, message: 'ok' } },
      { stdin: 'refresh-value' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ access_token: 'access-value', refresh_token: 'refresh-value' })
  })
})
