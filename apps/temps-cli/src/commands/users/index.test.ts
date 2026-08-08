import { test, expect, describe } from 'bun:test'
import { parseRolesInput } from './index.js'

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
