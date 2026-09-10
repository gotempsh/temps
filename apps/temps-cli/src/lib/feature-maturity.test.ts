// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import {
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
})
