// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

const root = new URL('../../', import.meta.url).pathname

async function read(path: string): Promise<string> {
  return Bun.file(`${root}${path}`).text()
}

// Lists whose Delete used to send the request on the first click.
const CONFIRMED_DELETE_LISTS = [
  'components/monitoring/AlertRulesManagement.tsx',
  'components/monitoring/NotificationRoutesManagement.tsx',
  'components/monitoring/ProvidersManagement.tsx',
  'components/email/EmailProvidersManagement.tsx',
  'components/email/EmailDomainsManagement.tsx',
]

// Save/delete handlers that used to `await mutateAsync(...)` with no catch,
// which turns a failed request into an unhandled promise rejection.
const MUTATION_HANDLER_FILES = [
  'components/monitoring/AlertRulesManagement.tsx',
  'components/monitoring/NotificationRoutesManagement.tsx',
  'components/monitoring/ProvidersManagement.tsx',
  'pages/AlertRuleForm.tsx',
  'pages/MetricAlertForm.tsx',
  'pages/MetricAlerts.tsx',
  'pages/NotificationRouteForm.tsx',
  'pages/EditNotificationProvider.tsx',
]

/** Source with // line comments blanked so prose cannot satisfy a check. */
function stripLineComments(source: string): string {
  return source.replace(/^\s*\/\/.*$/gm, '')
}

describe('delete actions ask for confirmation', () => {
  for (const path of CONFIRMED_DELETE_LISTS) {
    test(`${path} routes Delete through the confirmation dialog`, async () => {
      const source = stripLineComments(await read(path))
      expect(source).toContain('<ConfirmDeleteDialog')
      expect(source).toContain('useConfirmDelete')
      // The row/menu control may only request confirmation; the delete
      // mutation is reachable from the dialog's confirm handler alone.
      expect(source).not.toMatch(/onClick=\{[^}]*delete\w*Mutation\.mutate/i)
      expect(source).not.toMatch(/onClick=\{[^}]*handleDelete\(/)
      expect(source).toMatch(
        /onClick=\{\(\) =>\s*(?:onDelete|confirmDelete\.request)\(/
      )
    })
  }
})

describe('failed mutations never become unhandled rejections', () => {
  for (const path of MUTATION_HANDLER_FILES) {
    test(`${path} has no mutateAsync outside a try block`, async () => {
      const source = stripLineComments(await read(path))
      const offenders: string[] = []
      for (const match of source.matchAll(/\.mutateAsync\(/g)) {
        const before = source.slice(Math.max(0, match.index - 160), match.index)
        if (!/\btry\s*\{/.test(before)) {
          const line = source.slice(0, match.index).split('\n').length
          offenders.push(`${path}:${line}`)
        }
      }
      expect(offenders).toEqual([])
    })
  }
})
