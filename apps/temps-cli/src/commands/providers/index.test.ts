// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { resolveBitbucketAuthNonInteractive } from './index.js'
import { registerProvidersCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('resolveBitbucketAuthNonInteractive', () => {
  test('uses --password when --username and --password are both given', () => {
    expect(
      resolveBitbucketAuthNonInteractive({ username: 'dev', password: 'pw', token: 'tok' }),
    ).toEqual({ type: 'app_password', username: 'dev', password: 'pw' })
  })

  test('falls back to --token as the app password when --password is omitted', () => {
    expect(
      resolveBitbucketAuthNonInteractive({ username: 'dev', token: 'tok' }),
    ).toEqual({ type: 'app_password', username: 'dev', password: 'tok' })
  })

  test('rejects --username with neither --password nor --token', () => {
    // Without this guard the CLI would send an app_password auth body with
    // an empty password string instead of failing before the request.
    expect(() => resolveBitbucketAuthNonInteractive({ username: 'dev' })).toThrow(
      /Bitbucket app password is required/,
    )
  })

  test('uses an access token when --username is not given', () => {
    expect(resolveBitbucketAuthNonInteractive({ token: 'tok' })).toEqual({
      type: 'access_token',
      token: 'tok',
    })
  })

  test('rejects a call with neither --username nor --token', () => {
    expect(() => resolveBitbucketAuthNonInteractive({})).toThrow(
      /Bitbucket access token is required/,
    )
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

describe('connections update-token credential sources', () => {
  test('--token-file sends the file content exactly as the new token', async () => {
    const path = await credentialFile(CREDENTIAL_SENTINEL)
    const run = await runWithApi(registerProvidersCommands,
      ['providers', 'connections', 'update-token', '--id', '3', '--token-file', path],
      { 'POST /git-connections/3/update-token': { message: 'Token updated' } })
    expect(run.error).toBeUndefined()
    expect(credentialBody(run, 'POST /git-connections/3/update-token')).toEqual({ access_token: CREDENTIAL_SENTINEL })
    expect(`${run.stdout}\n${run.stderr}`).not.toContain(CREDENTIAL_SENTINEL)
  })

  test('--token-stdin reads the token from a pipe', async () => {
    const run = await runWithApi(registerProvidersCommands,
      ['providers', 'connections', 'update-token', '--id', '3', '--token-stdin', '--trim-trailing-newline'],
      { 'POST /git-connections/3/update-token': { message: 'Token updated' } }, { stdin: `${CREDENTIAL_SENTINEL}\n` })
    expect(run.error).toBeUndefined()
    expect(credentialBody(run, 'POST /git-connections/3/update-token')).toEqual({ access_token: CREDENTIAL_SENTINEL })
  })

  test('no token source fails before any request', async () => {
    const run = await runWithApi(registerProvidersCommands, ['providers', 'connections', 'update-token', '--id', '3'], {})
    expect(run.error?.message).toBe('Missing the new access token: pass --token-file <path>, --token-stdin, or -t/--token')
    expect(run.requests).toEqual([])
  })
})
