// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  DEPLOYMENT_TOKEN_PERMISSIONS,
  deploymentTokenErrorMessage,
  validateDeploymentTokenInput,
} from './deployment-token-form'

test('requires a name and at least one explicit deployment-token permission', () => {
  expect(validateDeploymentTokenInput('   ', '', ['kv:read'])).toBe(
    'Name is required.'
  )
  expect(validateDeploymentTokenInput('Worker', '', [])).toBe(
    'Select at least one permission.'
  )
  expect(validateDeploymentTokenInput('Worker', '', ['kv:read'])).toBeNull()
})

test('safely extracts deployment-token API errors', () => {
  expect(deploymentTokenErrorMessage({ detail: 'Permission denied' })).toBe(
    'Permission denied'
  )
  expect(deploymentTokenErrorMessage(new Error('Network unavailable'))).toBe(
    'Network unavailable'
  )
  expect(deploymentTokenErrorMessage(null)).toBe(
    'Failed to create deployment token.'
  )
})

test('rejects expired credentials before calling the API', () => {
  expect(
    validateDeploymentTokenInput('Worker', '2000-01-01T00:00', ['kv:read'])
  ).toBe('Expiration must be in the future.')
  expect(validateDeploymentTokenInput('Worker', 'invalid', ['kv:read'])).toBe(
    'Expiration must be in the future.'
  )
})

test('offers every permission accepted by the deployment-token service', () => {
  expect(DEPLOYMENT_TOKEN_PERMISSIONS.map(({ value }) => value)).toEqual([
    '*',
    'analytics:read',
    'events:write',
    'visitors:enrich',
    'emails:send',
    'errors:read',
    'ai_gateway:execute',
    'flags:read',
    'blob:read',
    'blob:write',
    'blob:delete',
    'kv:read',
    'kv:write',
    'kv:delete',
  ])
})
