// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import {
  maturityCommandName,
  maturityNotice,
  topLevelCommandName,
  type FeatureMaturity,
} from './feature-maturity'

const registry: FeatureMaturity[] = [
  {
    key: 'web-analytics',
    maturity: 'beta',
    reason: 'Analytics is still being hardened.',
    docs_path: 'https://temps.sh/docs/feature-maturity',
  },
  {
    key: 'project-groups',
    maturity: 'experimental',
    reason: 'Projects that group services are new.',
    docs_path: 'https://temps.sh/docs/feature-maturity',
  },
  {
    key: 'console-core',
    maturity: 'stable',
    reason: 'Stable.',
    docs_path: 'https://temps.sh/docs/feature-maturity',
  },
]

describe('CLI feature maturity notices', () => {
  test('formats a notice from the server registry', () => {
    const notice = maturityNotice('analytics', registry)
    expect(notice).toContain('[temps] Beta: analytics')
    expect(notice).toContain('Analytics is still being hardened.')
    expect(notice).toContain('https://temps.sh/docs/feature-maturity')
  })

  test('does not warn for unregistered command groups', () => {
    expect(maturityNotice('projects', registry)).toBeUndefined()
  })

  test('resolves a nested action to its top-level command group', () => {
    const root = new Command('temps')
    const analytics = root.command('analytics')
    const overview = analytics.command('overview')
    expect(topLevelCommandName(overview, root)).toBe('analytics')
  })

  test('marks projects (groups), imports, template creation and hosting as experimental', () => {
    const root = new Command('temps')
    const templates = root.command('templates')
    const create = templates.command('create')
    const list = templates.command('list')
    const projects = root.command('projects')
    const hosting = projects.command('hosting')
    const show = projects.command('show')
    const groups = root.command('project-groups')
    const varsSet = groups.command('vars').command('set')
    const inst = root.command('instantiations').command('list')

    expect(maturityCommandName(create, root)).toBe('templates create')
    expect(maturityCommandName(hosting, root)).toBe('projects hosting')
    expect(maturityCommandName(varsSet, root)).toBe('project-groups')
    expect(maturityCommandName(inst, root)).toBe('instantiations')
    for (const command of [create, hosting, varsSet, inst]) {
      const notice = maturityNotice(maturityCommandName(command, root), registry)
      expect(notice).toContain('Experimental')
    }
    // Their neighbours are not affected.
    expect(maturityNotice(maturityCommandName(list, root), registry)).toBeUndefined()
    expect(maturityNotice(maturityCommandName(show, root), registry)).toBeUndefined()
  })
})
