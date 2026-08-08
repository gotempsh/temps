import { test, expect, describe } from 'bun:test'
import { parseRolesInput } from './index.js'

describe('parseRolesInput', () => {
  test('defaults to viewer when no roles were given', () => {
    // Matches the API's own default: a user created without --roles must
    // not silently end up with zero roles.
    expect(parseRolesInput(undefined)).toEqual({ roles: ['viewer'] })
    expect(parseRolesInput('')).toEqual({ roles: ['viewer'] })
  })

  test('trims and lowercases a comma-separated list', () => {
    expect(parseRolesInput(' Admin , Developer ')).toEqual({ roles: ['admin', 'developer'] })
  })

  test('rejects an unknown role, naming it and the valid options', () => {
    expect(parseRolesInput('admin,root')).toEqual({
      error: 'Invalid role: root. Available roles: admin, developer, viewer',
    })
  })

  test('a single valid role round-trips without adding a default', () => {
    expect(parseRolesInput('developer')).toEqual({ roles: ['developer'] })
  })
})
