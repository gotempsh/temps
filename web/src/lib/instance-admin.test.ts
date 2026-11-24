// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { canDeleteProjects, isInstanceAdmin } from './instance-admin'

describe('isInstanceAdmin', () => {
  test('admin and platform_admin administer the instance', () => {
    expect(isInstanceAdmin('admin')).toBe(true)
    expect(isInstanceAdmin('platform_admin')).toBe(true)
  })

  test('every other role is refused admin-only reads', () => {
    for (const role of ['user', 'reader', 'api_reader', 'custom', 'demo']) {
      expect(isInstanceAdmin(role)).toBe(false)
    }
  })

  test('an unknown or missing role is not an admin', () => {
    expect(isInstanceAdmin(undefined)).toBe(false)
    expect(isInstanceAdmin(null)).toBe(false)
    expect(isInstanceAdmin('')).toBe(false)
    // Role names are matched exactly, as the server serialises them.
    expect(isInstanceAdmin('Admin')).toBe(false)
  })
})

describe('canDeleteProjects', () => {
  test('only admin holds projects:delete', () => {
    expect(canDeleteProjects('admin')).toBe(true)
  })

  test('platform_admin administers the instance but cannot delete projects', () => {
    expect(isInstanceAdmin('platform_admin')).toBe(true)
    expect(canDeleteProjects('platform_admin')).toBe(false)
  })

  test('every other or missing role cannot delete projects', () => {
    for (const role of [
      'user',
      'reader',
      'api_reader',
      'custom',
      'Admin',
      '',
    ]) {
      expect(canDeleteProjects(role)).toBe(false)
    }
    expect(canDeleteProjects(undefined)).toBe(false)
    expect(canDeleteProjects(null)).toBe(false)
  })
})
