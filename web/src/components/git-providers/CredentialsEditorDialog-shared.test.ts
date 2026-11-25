// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { ProviderResponse } from '@/api/client/types.gen'
import {
  fieldsForProvider,
  providerHasEditableCredentials,
} from './CredentialsEditorDialog-shared'

function provider(provider_type: string, auth_method: string): ProviderResponse {
  return {
    id: 1,
    name: 'provider',
    provider_type,
    auth_method,
    base_url: null,
    is_active: true,
    is_default: false,
    created_at: '2026-11-25T11:39:26Z',
    updated_at: '2026-11-25T11:39:26Z',
  } as ProviderResponse
}

describe('fieldsForProvider', () => {
  it('lets a basic-auth provider rotate its username and password', () => {
    const keys = fieldsForProvider(provider('generic', 'basic')).map((f) => f.key)
    expect(keys).toEqual(['username', 'password'])
    expect(providerHasEditableCredentials(provider('bitbucket', 'basic'))).toBe(true)
  })

  it('lets an SSH-key provider replace its key pair', () => {
    const keys = fieldsForProvider(provider('generic', 'ssh')).map((f) => f.key)
    expect(keys).toEqual(['private_key', 'public_key'])
  })

  it('keeps the password field masked', () => {
    const password = fieldsForProvider(provider('generic', 'basic')).find(
      (f) => f.key === 'password'
    )
    expect(password?.type).toBe('password')
  })
})
