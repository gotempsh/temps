// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { resolveEmailProviderCredentials } from './index.js'

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
