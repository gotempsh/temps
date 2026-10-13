// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { HttpCheckView } from '@/api/client'
import {
  LOCAL_PROVIDER,
  checkSourceLabel,
  checksFor,
  credentialSource,
  describeArtifact,
  historyEventName,
  indicatorsBySubject,
  markProvider,
  parseWarningDays,
  scopeSummary,
} from './http-checks'

const check = (overrides: Partial<HttpCheckView>): HttpCheckView => ({
  id: 1,
  project_id: 10,
  env_var_id: null,
  secret_id: null,
  kind: 'http',
  name: 'Check',
  automatic_provider: null,
  enabled: true,
  interval_seconds: 86400,
  next_check_at: '2026-10-03T00:00:00Z',
  last_checked_at: null,
  result: null,
  ...overrides,
})

describe('credential subjects', () => {
  const variableCheck = check({ id: 1, env_var_id: 5 })
  const secretCheck = check({ id: 2, secret_id: 5, kind: 'local' })

  test('a variable and a secret with the same id never share checks', () => {
    const checks = [variableCheck, secretCheck]
    expect(checksFor(checks, { kind: 'env_var', id: 5 })).toEqual([
      variableCheck,
    ])
    expect(checksFor(checks, { kind: 'secret', id: 5 })).toEqual([secretCheck])
  })

  test('new checks bind exactly one credential source', () => {
    expect(
      credentialSource({ kind: 'secret', id: 7, key: 'TLS_CERT' })
    ).toEqual({ env_var_id: null, secret_id: 7, credential: null })
    expect(
      credentialSource({ kind: 'env_var', id: 3, key: 'API_TOKEN' })
    ).toEqual({ env_var_id: 3, secret_id: null, credential: null })
  })

  test('list indicators are grouped per subject kind', () => {
    const grouped = indicatorsBySubject([variableCheck, secretCheck], 'secret')
    expect([...grouped.keys()]).toEqual([5])
    expect(grouped.get(5)?.[0].id).toBe('2')
  })
})

describe('warning thresholds', () => {
  test('accepts the server rule and normalises order and duplicates', () => {
    expect(parseWarningDays('30, 7, 1')).toEqual([30, 7, 1])
    expect(parseWarningDays('1 7 7 30')).toEqual([30, 7, 1])
    expect(parseWarningDays('14')).toEqual([14])
  })

  test('rejects values the server would reject', () => {
    for (const input of ['', '0', '366', '7.5', 'soon', '1,2,3,4,5,6,7,8,9'])
      expect(parseWarningDays(input)).toBeNull()
  })
})

describe('labels', () => {
  test('names automatic, local and HTTP checks distinctly', () => {
    expect(
      checkSourceLabel({
        automatic_provider: LOCAL_PROVIDER,
        kind: 'local',
      })
    ).toBe('Automatic expiry check')
    expect(
      checkSourceLabel({ automatic_provider: 'github', kind: 'http' })
    ).toBe('Automatic detection')
    expect(checkSourceLabel({ automatic_provider: null, kind: 'local' })).toBe(
      'Local expiry check'
    )
    expect(checkSourceLabel({ automatic_provider: null, kind: 'http' })).toBe(
      'Custom HTTP check'
    )
  })

  test('every local expiry check gets the local mark, manual or automatic', () => {
    expect(markProvider({ kind: 'local', automatic_provider: null })).toBe(
      LOCAL_PROVIDER
    )
    expect(
      markProvider({ kind: 'local', automatic_provider: LOCAL_PROVIDER })
    ).toBe(LOCAL_PROVIDER)
    expect(markProvider({ kind: 'http', automatic_provider: 'github' })).toBe(
      'github'
    )
    expect(markProvider({ kind: 'http', automatic_provider: null })).toBe(null)
  })

  test('describes an expiring item by its label and UTC expiry date', () => {
    expect(
      describeArtifact({
        label: "Kubeconfig user 'ci' client certificate 'ci-runner'",
        expires_at: '2027-01-04T00:00:00Z',
      })
    ).toBe(
      "Kubeconfig user 'ci' client certificate 'ci-runner' · expires 2027-01-04"
    )
  })

  test('summarises secret scope changes', () => {
    expect(scopeSummary({})).toBeNull()
    expect(scopeSummary({ environment_ids: [], compose_services: [] })).toBe(
      'All environments'
    )
    expect(
      scopeSummary({
        environment_ids: [3],
        compose_services: ['api', 'worker'],
      })
    ).toBe('1 environment · only api, worker')
  })
})

describe('history events', () => {
  test('name events for the subject and fall back for unknown kinds', () => {
    expect(historyEventName('created', 'secret')).toBe('Secret created')
    expect(historyEventName('created', 'env_var')).toBe('Variable created')
    expect(historyEventName('scope_changed', 'secret')).toBe(
      'Access scope changed'
    )
    expect(historyEventName('something_new', 'secret')).toBe('Secret activity')
  })
})
