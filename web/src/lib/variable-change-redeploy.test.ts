// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { EnvironmentResponse } from '@/api/client'
import {
  combinedScope,
  environmentsAffectedByChange,
  variableChangeRedeployMessage,
} from './variable-change-redeploy'

const env = (
  id: number,
  name: string,
  current: number | null,
  isPreview = false
): EnvironmentResponse =>
  ({
    id,
    name,
    current_deployment_id: current,
    is_preview: isPreview,
  }) as EnvironmentResponse

const environments = [
  env(1, 'production', 10),
  env(2, 'staging', 20),
  env(3, 'qa', null),
  env(4, 'feature-x', 40, true),
]

describe('environmentsAffectedByChange', () => {
  test('keeps only scoped environments that are running a deployment', () => {
    expect(
      environmentsAffectedByChange(environments, [1, 3]).map((e) => e.name)
    ).toEqual(['production'])
  })

  test('an unscoped change affects every running non-preview environment', () => {
    expect(
      environmentsAffectedByChange(environments, 'all').map((e) => e.name)
    ).toEqual(['production', 'staging'])
  })

  test('nothing to redeploy when nothing in scope is live', () => {
    expect(environmentsAffectedByChange(environments, [])).toEqual([])
    expect(environmentsAffectedByChange(environments, [3])).toEqual([])
    expect(environmentsAffectedByChange(undefined, 'all')).toEqual([])
  })
})

describe('combinedScope', () => {
  test('merges and de-duplicates ids', () => {
    expect(combinedScope([[1, 2], [2, 3], []])).toEqual([1, 2, 3])
  })

  test('any unscoped item makes the whole change unscoped', () => {
    expect(combinedScope([[1], 'all'])).toBe('all')
  })
})

describe('variableChangeRedeployMessage', () => {
  test('names the single environment to redeploy', () => {
    expect(variableChangeRedeployMessage([{ name: 'production' }])).toBe(
      'Applies on next deploy. Redeploy production to apply it now.'
    )
  })

  test('counts and lists several environments', () => {
    expect(
      variableChangeRedeployMessage([
        { name: 'production' },
        { name: 'staging' },
      ])
    ).toBe(
      'Applies on next deploy. Redeploy 2 environments (production, staging) to apply it now.'
    )
  })

  test('still says when it applies when nothing is running', () => {
    expect(variableChangeRedeployMessage([])).toBe('Applies on next deploy.')
  })
})
