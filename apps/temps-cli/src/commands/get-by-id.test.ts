// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The read-one commands for the get-by-id operations the API gained. */

import { describe, expect, test } from 'bun:test'
import type { Command } from 'commander'
import { registerAnalyticsCommands } from './analytics/index.js'
import { registerEndpointCommands } from './endpoints.js'
import { registerErrorsCommands } from './errors/index.js'
import { registerProjectGroupsCommands } from './project-groups/index.js'
import { registerProjectsCommands } from './projects/index.js'
import { registerServerCommands } from './server/index.js'
import { registerStatusPagesCommands } from './status-pages/index.js'
import { registerTeamsCommands } from './teams/index.js'
import { registerUsersCommands } from './users/index.js'
import { runWithApi } from '../lib/mock-api.js'

const cases: [string, (program: Command) => void, string[], string][] = [
  ['shield get-entry', registerEndpointCommands, ['shield', 'get-entry', '5', '-p', '7'], 'GET /projects/7/shield/entries/5'],
  ['shield get-platform-entry', registerEndpointCommands, ['shield', 'get-platform-entry', '5'], 'GET /settings/shield/entries/5'],
  ['monitors http-checks get', registerEndpointCommands, ['monitors', 'http-checks', 'get', '3', '-p', '7'], 'GET /projects/7/http-checks/3'],
  ['settings oidc get-provider', registerEndpointCommands, ['settings', 'oidc', 'get-provider', '2'], 'GET /admin/oidc/providers/2'],
  ['server alert-rules get', registerServerCommands, ['server', 'alert-rules', 'get', '0', '9'], 'GET /nodes/0/metrics/alert-rules/9'],
  ['project-groups vars get', registerProjectGroupsCommands, ['project-groups', 'vars', 'get', '4', '11'], 'GET /project-groups/4/env-vars/11'],
  ['analytics keys get', registerAnalyticsCommands, ['analytics', 'keys', 'get', '6', '-p', '7'], 'GET /projects/7/analytics/ingest-keys/6'],
  ['errors filters get', registerErrorsCommands, ['errors', 'filters', 'get', '8', '-p', '7'], 'GET /projects/7/error-inbound-filters/8'],
  ['projects secrets get', registerProjectsCommands, ['projects', 'secrets', 'get', '12', '-p', '7'], 'GET /projects/7/secrets/12'],
  ['status-pages webhooks get', registerStatusPagesCommands, ['status-pages', 'webhooks', 'get', '1', '2'], 'GET /status-pages/1/webhooks/2'],
  ['teams members get', registerTeamsCommands, ['teams', 'members', 'get', '3', '42'], 'GET /teams/3/members/42'],
  ['users get', registerUsersCommands, ['users', 'get', '42'], 'GET /users/42'],
]

describe('get-by-id commands', () => {
  test.each(cases)('%s calls its operation and prints the record', async (_name, register, argv, request) => {
    const run = await runWithApi(register, [...argv, '--json'], { [request]: { id: 1 } })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([request])
    expect(JSON.parse(run.stdout)).toEqual({ id: 1 })
  })

  test('oidc list-provider-users passes paging', async () => {
    const run = await runWithApi(
      registerEndpointCommands,
      ['settings', 'oidc', 'list-provider-users', '2', '--page', '3', '--page-size', '50', '--json'],
      { 'GET /admin/oidc/providers/2/users': { users: [] } },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /admin/oidc/providers/2/users?page=3&page_size=50'])
  })
})
