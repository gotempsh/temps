// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ingressStatusSummary,
  type IngressSettings,
} from './environment-ingress'

function settings(overrides: Partial<IngressSettings>): IngressSettings {
  return {
    project_id: 1,
    environment_id: 2,
    mode: 'legacy',
    require_login: false,
    login_permission: null,
    login_enforced: false,
    ...overrides,
  }
}

describe('ingressStatusSummary', () => {
  test('says a saved login waits for enforce mode', () => {
    expect(
      ingressStatusSummary(
        settings({ require_login: true, login_permission: 'projects:read' })
      )
    ).toContain('only applies in Enforce mode')
  })

  test('says when visitors must sign in', () => {
    expect(
      ingressStatusSummary(
        settings({
          mode: 'enforce',
          require_login: true,
          login_permission: 'projects:read',
          login_enforced: true,
        })
      )
    ).toBe('The compiled endpoint decides, and visitors must sign in to Temps.')
  })

  test('describes shadow mode as comparing', () => {
    expect(ingressStatusSummary(settings({ mode: 'shadow' }))).toContain(
      'disagreements are counted'
    )
  })
})
