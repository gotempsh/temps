import { test, expect, describe } from 'bun:test'
import { parseRole, resolveProjectId, resolveTeamId, resolveUserId, slugify } from './index.js'

describe('slugify', () => {
  test('lowercases and replaces non-alphanumeric runs with a single dash', () => {
    expect(slugify('My Team!')).toBe('my-team')
    expect(slugify('Platform  &  Infra')).toBe('platform-infra')
  })

  test('strips leading and trailing dashes', () => {
    expect(slugify('--Ops--')).toBe('ops')
  })

  test('truncates to 64 characters', () => {
    const long = 'a'.repeat(100)
    expect(slugify(long).length).toBe(64)
  })
})

describe('parseRole', () => {
  test('lowercases a valid role', () => {
    expect(parseRole('ADMIN')).toBe('admin')
    expect(parseRole('viewer')).toBe('viewer')
  })

  test('rejects a role outside the known set, naming the value and the choices', () => {
    // A silently-accepted bad role would grant/deny the wrong permissions.
    expect(() => parseRole('root')).toThrow(
      "Invalid role 'root'. Expected one of: owner, admin, deployer, viewer",
    )
  })
})

describe('resolveTeamId / resolveUserId / resolveProjectId numeric fast path', () => {
  test('a purely numeric team ref resolves without hitting the API', async () => {
    // No credentials/client mocking is set up here — if this ever fell
    // through to the slug-lookup branch it would throw, proving the fast
    // path is what actually ran.
    expect(await resolveTeamId('42')).toBe(42)
  })

  test('a purely numeric user ref resolves without hitting the API', async () => {
    expect(await resolveUserId('7')).toBe(7)
  })

  test('a numeric --project flag resolves without hitting the API', async () => {
    expect(await resolveProjectId('123')).toBe(123)
  })
})
