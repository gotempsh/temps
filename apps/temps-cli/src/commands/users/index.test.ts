// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parseRolesInput, parseUserId } from './index.js'

describe('parseRolesInput', () => {
  test('defaults to user when no roles were given', () => {
    // Matches the API's own default: a user created without --roles must
    // not silently end up with zero roles.
    expect(parseRolesInput(undefined)).toEqual({ roles: ['user'] })
    expect(parseRolesInput('')).toEqual({ roles: ['user'] })
  })

  test('trims and lowercases a comma-separated list', () => {
    expect(parseRolesInput(' Admin , User ')).toEqual({ roles: ['admin', 'user'] })
  })

  test('rejects an unknown role, naming it and the valid options', () => {
    expect(parseRolesInput('admin,root')).toEqual({
      error: 'Invalid role: root. Available roles: admin, user',
    })
  })

  test('rejects team-role names -- those are a distinct concept (see registerTeamsCommands), not instance-wide user roles', () => {
    expect(parseRolesInput('developer')).toEqual({
      error: 'Invalid role: developer. Available roles: admin, user',
    })
    expect(parseRolesInput('viewer')).toEqual({
      error: 'Invalid role: viewer. Available roles: admin, user',
    })
  })

  test('a single valid role round-trips without adding a default', () => {
    expect(parseRolesInput('admin')).toEqual({ roles: ['admin'] })
  })
})

describe('parseUserId', () => {
  test('accepts a positive whole number, ignoring surrounding whitespace', () => {
    expect(parseUserId('12')).toBe(12)
    expect(parseUserId(' 7 ')).toBe(7)
  })

  test('rejects numeric prefixes that parseInt would silently truncate', () => {
    // parseInt('12x') === 12 and parseInt('12.9') === 12: a typo must not
    // select user 12 for a destructive command.
    for (const raw of ['12x', '12.9', '1e3', '0x10', '12 13']) {
      expect(parseUserId(raw)).toBeNull()
    }
  })

  test('rejects zero, negatives, empty input and values beyond i32', () => {
    for (const raw of ['0', '-3', '', '   ', '007', '2147483648']) {
      expect(parseUserId(raw)).toBeNull()
    }
  })
})
