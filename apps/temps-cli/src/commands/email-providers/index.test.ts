// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { resolveEmailProviderCredentials } from './index.js'
import { registerEmailProvidersCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('resolveEmailProviderCredentials', () => {
  describe('ses', () => {
    test('builds ses_credentials and leaves scaleway_credentials null', () => {
      expect(
        resolveEmailProviderCredentials('ses', {
          accessKeyId: 'AKIA',
          secretAccessKey: 'shh',
        }),
      ).toEqual({
        ses_credentials: { access_key_id: 'AKIA', secret_access_key: 'shh' },
        scaleway_credentials: null,
      })
    })

    test('defers to interactive prompts when no flags and no --yes', () => {
      expect(resolveEmailProviderCredentials('ses', {})).toBeUndefined()
    })

    test('requires both key fields under --yes, not just one', () => {
      expect(() =>
        resolveEmailProviderCredentials('ses', { accessKeyId: 'AKIA', yes: true }),
      ).toThrow('--access-key-id and --secret-access-key are required for SES when using --yes flag')
    })
  })

  describe('scaleway', () => {
    test('builds scaleway_credentials and leaves ses_credentials null', () => {
      expect(
        resolveEmailProviderCredentials('scaleway', {
          apiKey: 'scw-key',
          projectId: 'proj-1',
        }),
      ).toEqual({
        ses_credentials: null,
        scaleway_credentials: { api_key: 'scw-key', project_id: 'proj-1' },
      })
    })

    test('defers to interactive prompts when no flags and no --yes', () => {
      expect(resolveEmailProviderCredentials('scaleway', {})).toBeUndefined()
    })

    test('requires both fields under --yes, not just one', () => {
      expect(() =>
        resolveEmailProviderCredentials('scaleway', { apiKey: 'scw-key', yes: true }),
      ).toThrow('--api-key and --project-id are required for Scaleway when using --yes flag')
    })
  })

  test('an unsupported provider type falls through to undefined rather than crashing', () => {
    // The type-route union includes 'smtp', which this CLI does not implement
    // a credential shape for — the caller rejects it earlier with a warning,
    // so this function must not throw or fabricate credentials for it.
    expect(resolveEmailProviderCredentials('smtp', { yes: true })).toBeUndefined()
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

describe('email-providers create credential sources', () => {
  test('--secret-access-key-file sends the key from the file exactly', async () => {
    const path = await credentialFile(CREDENTIAL_SENTINEL)
    const run = await runWithApi(registerEmailProvidersCommands,
      ['email-providers', 'create', '-n', 'ses', '-t', 'ses', '-r', 'us-east-1', '--access-key-id', 'AKIAEXAMPLE', '--secret-access-key-file', path, '-y'],
      { 'POST /email-providers': { id: 1, name: 'ses', provider_type: 'ses', region: 'us-east-1', is_active: true, created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' } })
    expect(run.error).toBeUndefined()
    const body = credentialBody(run, 'POST /email-providers') as { ses_credentials: { secret_access_key: unknown } }
    expect(body.ses_credentials.secret_access_key).toBe(CREDENTIAL_SENTINEL)
  })
})

describe('email-providers create prompts only for what is missing', () => {
  test('a --secret-access-key-file value is kept when --access-key-id is prompted for', async () => {
    const prompts = await import('../../ui/prompts.js')
    const asked: string[] = []
    const text = spyOn(prompts, 'promptText').mockImplementation(async (options) => {
      asked.push(options.message)
      return 'AKIAEXAMPLE'
    })
    const password = spyOn(prompts, 'promptPassword').mockImplementation(async (options) => {
      asked.push(options.message)
      return 'typed-instead'
    })
    try {
      const path = await credentialFile(CREDENTIAL_SENTINEL)
      const run = await runWithApi(registerEmailProvidersCommands,
        ['email-providers', 'create', '-n', 'ses', '-t', 'ses', '-r', 'us-east-1', '--secret-access-key-file', path],
        { 'POST /email-providers': { id: 1, name: 'ses', provider_type: 'ses', region: 'us-east-1', is_active: true, created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' } })
      expect(run.error).toBeUndefined()
      expect(asked).toEqual(['AWS Access Key ID'])
      const body = credentialBody(run, 'POST /email-providers') as { ses_credentials: Record<string, unknown> }
      expect(body.ses_credentials).toEqual({ access_key_id: 'AKIAEXAMPLE', secret_access_key: CREDENTIAL_SENTINEL })

      // An empty --access-key-id counts as missing and is prompted for, as
      // the non-interactive resolver already treats it.
      asked.length = 0
      const empty = await runWithApi(registerEmailProvidersCommands,
        ['email-providers', 'create', '-n', 'ses', '-t', 'ses', '-r', 'us-east-1', '--access-key-id', '', '--secret-access-key-file', path],
        { 'POST /email-providers': { id: 1, name: 'ses', provider_type: 'ses', region: 'us-east-1', is_active: true, created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' } })
      expect(empty.error).toBeUndefined()
      expect(asked).toEqual(['AWS Access Key ID'])
      expect((credentialBody(empty, 'POST /email-providers') as { ses_credentials: Record<string, unknown> }).ses_credentials.access_key_id).toBe('AKIAEXAMPLE')
    } finally {
      text.mockRestore()
      password.mockRestore()
    }
  })
})

test('masked credential prompts refuse an empty answer', async () => {
  const { requireNonEmpty } = await import('./index.js')
  expect(requireNonEmpty('')).toBe('A value is required')
  expect(requireNonEmpty('s3cr3t-test-value')).toBe(true)
})
