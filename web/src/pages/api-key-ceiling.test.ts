// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  canGrantPermission,
  canGrantRole,
  permissionCeiling,
} from './api-key-ceiling'

const roles = [
  { name: 'admin', permissions: ['a:read', 'a:write', 'b:write'] },
  { name: 'user', permissions: ['a:read', 'a:write'] },
  { name: 'reader', permissions: ['a:read'] },
]

describe('permissionCeiling', () => {
  test("is the signed-in role's own permissions", () => {
    expect([...(permissionCeiling(roles, 'user') ?? [])]).toEqual([
      'a:read',
      'a:write',
    ])
  })

  test('is unknown, so nothing is filtered, when the role cannot be read', () => {
    expect(permissionCeiling(undefined, 'user')).toBeUndefined()
    expect(permissionCeiling(roles, undefined)).toBeUndefined()
    expect(permissionCeiling(roles, 'not-in-catalogue')).toBeUndefined()
    // A custom role's permissions are per user, not in the catalogue.
    expect(permissionCeiling(roles, 'custom')).toBeUndefined()
  })
})

describe('canGrantRole / canGrantPermission', () => {
  test('a role is grantable only when every permission is held', () => {
    const ceiling = permissionCeiling(roles, 'user')
    expect(canGrantRole(roles[0], ceiling)).toBe(false)
    expect(canGrantRole(roles[1], ceiling)).toBe(true)
    expect(canGrantRole(roles[2], ceiling)).toBe(true)
  })

  test('a single permission is grantable only when held', () => {
    const ceiling = permissionCeiling(roles, 'user')
    expect(canGrantPermission('a:write', ceiling)).toBe(true)
    expect(canGrantPermission('b:write', ceiling)).toBe(false)
  })

  test('with an unknown ceiling everything is offered and the server decides', () => {
    expect(canGrantRole(roles[0], undefined)).toBe(true)
    expect(canGrantPermission('b:write', undefined)).toBe(true)
  })
})
