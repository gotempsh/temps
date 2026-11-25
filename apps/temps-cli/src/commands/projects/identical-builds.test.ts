// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { parseMode, parseScope } from './identical-builds.js'
import { registerProjectsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const settings = {
  mode: 'parallel',
  scope: 'source',
  modes: [
    { id: 'parallel', label: 'Build every deployment', description: 'd', example: 'e' },
    { id: 'share', label: 'Build once and share the image', description: 'd', example: 'e' },
  ],
  scopes: [{ id: 'source', label: 'Same repository and path', description: 'd' }],
  reuse_window_seconds: 1800,
  max_wait_seconds: 5400,
  activity: { reused: 2, built: 1, waiting: 0, building: 0 },
  can_edit: true,
}

describe('parseMode and parseScope', () => {
  test('accept the documented values in any case', () => {
    expect(parseMode('Share')).toBe('share')
    expect(parseMode(' queue ')).toBe('queue')
    expect(parseScope('SOURCE')).toBe('source')
  })

  test('reject anything else, listing what is accepted', () => {
    expect(() => parseMode('always')).toThrow(/parallel, queue, share/)
    expect(() => parseScope('repo')).toThrow(/source, fingerprint/)
  })
})

describe('projects identical-builds', () => {
  test('shows the setting without changing it', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'identical-builds', '--project', '12', '--json'],
      {
        'GET /projects/12': { id: 12, slug: 'web', name: 'web' },
        'GET /projects/12/identical-builds': settings,
      }
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /projects/12', 'GET /projects/12/identical-builds'])
    const output = JSON.parse(run.stdout)
    expect(output.project).toBe('web')
    expect(output.activity.reused).toBe(2)
  })

  test('changes the mode and keeps the saved scope', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'identical-builds', '--project', '12', '--mode', 'share', '--json'],
      {
        'GET /projects/12': { id: 12, slug: 'web', name: 'web' },
        'GET /projects/12/identical-builds': settings,
        'PUT /projects/12/identical-builds': { ...settings, mode: 'share' },
      }
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([
      'GET /projects/12',
      'GET /projects/12/identical-builds',
      'PUT /projects/12/identical-builds',
    ])
    expect(JSON.parse(run.stdout).mode).toBe('share')
  })

  test('refuses an unknown mode before calling the API', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'identical-builds', '--project', '12', '--mode', 'sometimes'],
      {}
    )
    expect(String(run.error)).toContain('not an identical build mode')
    expect(run.requests).toEqual([])
  })
})
