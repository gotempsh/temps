// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { existsSync } from 'node:fs'
import {
  monitoringSetupPrompt,
  monitoringSetupSkills,
} from './monitoring-setup-prompt'

for (const feature of ['analytics', 'errors', 'traces'] as const) {
  test(`${feature} prompt links an existing skill and preserves project context`, () => {
    const skill = monitoringSetupSkills[feature]
    expect(existsSync(`../skills/${skill}/SKILL.md`)).toBe(true)
    const prompt = monitoringSetupPrompt(
      feature,
      { id: 42, name: 'My app', slug: 'my-app' },
      'http://localhost:3000'
    )
    expect(prompt).toContain(`--skill ${skill}`)
    expect(prompt).toContain('http://localhost:3000/projects/my-app/')
    expect(prompt).toContain('"id":42')
    expect(prompt).toContain('Do not create another Temps project or deploy it')
    expect(prompt).toContain('never invent them')
    expect(prompt).toContain('confirm that it appears')
  })
}
