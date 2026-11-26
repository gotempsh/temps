// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerSkillsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const skill = { slug: 'review', name: 'Review', description: 'Reviews code' }

describe('temps skills update', () => {
  test('sends a partial PATCH for a global skill (PUT is the deprecated alias)', async () => {
    const run = await runWithApi(registerSkillsCommands, ['skills', 'update', 'review', '--global', '-d', 'New text'], {
      'PATCH /settings/skills/review': skill,
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([{ request: 'PATCH /settings/skills/review', body: { description: 'New text' } }])
  })

  test('sends a PATCH for a project skill', async () => {
    const run = await runWithApi(registerSkillsCommands, ['skills', 'update', 'review', '--project', 'web', '-n', 'Renamed'], {
      'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' },
      'PATCH /projects/7/skills/review': skill,
    })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /projects/by-slug/web', 'PATCH /projects/7/skills/review'])
    expect(run.bodies[0]?.body).toEqual({ name: 'Renamed' })
  })
})
