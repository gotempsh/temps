// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { nextCapabilities, normalizeCapability } from './container-security.js'
import { registerProjectsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('normalizeCapability', () => {
  test('accepts the Docker, kernel and setcap spellings', () => {
    expect(normalizeCapability('NET_BIND_SERVICE')).toBe('NET_BIND_SERVICE')
    expect(normalizeCapability('cap_net_bind_service')).toBe('NET_BIND_SERVICE')
    expect(normalizeCapability(' CAP_NET_BIND_SERVICE ')).toBe('NET_BIND_SERVICE')
  })

  test('rejects anything that is not a capability name', () => {
    expect(() => normalizeCapability('NET BIND')).toThrow(/not a Linux capability/)
    expect(() => normalizeCapability('')).toThrow(/not a Linux capability/)
    expect(() => normalizeCapability('--privileged')).toThrow(/not a Linux capability/)
  })
})

describe('nextCapabilities', () => {
  test('adds and removes, sorted and without duplicates', () => {
    expect(nextCapabilities([], ['NET_BIND_SERVICE'], [])).toEqual(['NET_BIND_SERVICE'])
    expect(nextCapabilities(['NET_BIND_SERVICE'], ['NET_BIND_SERVICE'], [])).toEqual(['NET_BIND_SERVICE'])
    expect(nextCapabilities(['NET_BIND_SERVICE'], [], ['NET_BIND_SERVICE'])).toEqual([])
  })

  test('refuses to allow and remove the same capability', () => {
    expect(() => nextCapabilities([], ['NET_BIND_SERVICE'], ['NET_BIND_SERVICE'])).toThrow(/both allow and remove/)
  })
})

describe('projects container-security --project', () => {
  const security = {
    allowed_capabilities: [],
    options: [],
    always_enforced: [],
    can_edit: true,
    edit_requirement: 'An instance administrator can change it.',
  }

  test('reads a numeric value as the project ID, never as a slug', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'container-security', '--project', '12', '--json'],
      {
        'GET /projects/12': { id: 12, slug: 'web', name: 'web' },
        'GET /projects/12/container-security': security,
      }
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /projects/12', 'GET /projects/12/container-security'])
    expect(JSON.parse(run.stdout).project).toBe('web')
  })

  test('looks any other value up by slug', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'container-security', '--project', '12-web', '--json'],
      {
        'GET /projects/by-slug/12-web': { id: 3, slug: '12-web', name: '12-web' },
        'GET /projects/3/container-security': security,
      }
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /projects/by-slug/12-web', 'GET /projects/3/container-security'])
  })
})
