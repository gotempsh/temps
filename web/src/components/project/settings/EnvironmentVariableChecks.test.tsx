// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  EnvironmentVariableChecks,
  type EnvironmentVariableCheck,
} from './EnvironmentVariableChecks'

const check = (
  status: EnvironmentVariableCheck['status'],
  label: string
): EnvironmentVariableCheck => ({
  id: label,
  status,
  label,
  detail: 'HTTP check result.',
})

describe('environment variable check indicators', () => {
  test('shows green no-issues when there are no checks', () => {
    const html = renderToStaticMarkup(<EnvironmentVariableChecks />)
    expect(html).toContain('Checks: No issues')
    expect(html).toContain('text-emerald-700')
  })

  test('shows the warning instead of a successful check', () => {
    const html = renderToStaticMarkup(
      <EnvironmentVariableChecks
        checks={[
          check('healthy', 'HTTP 200'),
          check('warning', 'Expires in 7 days'),
        ]}
      />
    )
    expect(html).toContain('Checks: Expires in 7 days')
    expect(html).toContain('text-amber-700')
    expect(html).not.toContain('No issues')
  })

  test('counts issues and prioritizes errors regardless of order', () => {
    for (const checks of [
      [check('warning', 'Low balance'), check('error', 'HTTP 401')],
      [check('error', 'HTTP 401'), check('warning', 'Low balance')],
    ]) {
      const html = renderToStaticMarkup(
        <EnvironmentVariableChecks checks={checks} />
      )
      expect(html).toContain('Checks: 2 issues')
      expect(html).toContain('text-red-700')
    }
  })

  test.each(['pending', 'unknown'] as const)(
    '%s checks do not appear healthy',
    (status) => {
      const html = renderToStaticMarkup(
        <EnvironmentVariableChecks
          checks={[
            check('healthy', 'HTTP 200'),
            check(status, 'Awaiting result'),
          ]}
        />
      )
      expect(html).toContain('Checks: Awaiting result')
      expect(html).not.toContain('No issues')
      expect(html).not.toContain('text-emerald-700')
    }
  )
})
