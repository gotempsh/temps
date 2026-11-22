// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parseRolesInput, parseUserId } from './index.js'
import { registerUsersCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

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

// --- Credential file/stdin sources (shared value-source helper) ---

const CREDENTIAL_SENTINEL = 's3cr3t-test-value'

async function credentialFile(contents: string): Promise<string> {
  const { mkdtemp, writeFile, chmod } = await import('node:fs/promises')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  const path = join(await mkdtemp(join(tmpdir(), 'temps-credential-')), 'credential.txt')
  await writeFile(path, contents)
  await chmod(path, 0o600)
  return path
}

function credentialBody(run: { bodies: Array<{ request: string; body: unknown }> }, request: string): Record<string, unknown> {
  const sent = run.bodies.find((b) => b.request === request)
  expect(sent).toBeDefined()
  return sent!.body as Record<string, unknown>
}

describe('users create credential sources', () => {
  test('--password-stdin sends the piped password exactly', async () => {
    const run = await runWithApi(registerUsersCommands,
      ['users', 'create', '-u', 'alice', '-e', 'alice@example.com', '--password-stdin', '-y'],
      { 'POST /users': (_url: URL, body: unknown) => ({ id: 2, username: 'alice', email: 'alice@example.com', roles: [], ...(body as object) }) },
      { stdin: CREDENTIAL_SENTINEL })
    expect(run.error).toBeUndefined()
    expect(credentialBody(run, 'POST /users').password).toBe(CREDENTIAL_SENTINEL)
  })
})
