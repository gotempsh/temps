// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { SecretResponse } from '@/api/client/types.gen'
import { secretScopeLabel, secretScopeQuery } from './secretScope'

function secret(project_id: number | null): SecretResponse {
  return {
    id: 1,
    project_id,
    name: 'API_TOKEN',
    secret_type: 'env',
    value: '***',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  }
}

test('a project secret is edited and deleted in its own project scope', () => {
  expect(secretScopeQuery(secret(7))).toEqual({ project_id: 7 })
  expect(secretScopeLabel(secret(7))).toBe('Project #7')
})

test('an instance-wide secret sends no scope selector', () => {
  expect(secretScopeQuery(secret(null))).toBeUndefined()
  expect(secretScopeLabel(secret(null))).toBe('Instance-wide')
})
