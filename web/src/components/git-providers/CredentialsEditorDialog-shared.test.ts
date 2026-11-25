// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { ProviderResponse } from '@/api/client/types.gen'
import {
  credentialFieldErrors,
  fieldsForProvider,
  providerHasEditableCredentials,
} from './CredentialsEditorDialog-shared'

function provider(
  provider_type: string,
  auth_method: string
): ProviderResponse {
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
    const keys = fieldsForProvider(provider('generic', 'basic')).map(
      (f) => f.key
    )
    expect(keys).toEqual(['username', 'password'])
    expect(providerHasEditableCredentials(provider('bitbucket', 'basic'))).toBe(
      true
    )
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

describe('credentialFieldErrors', () => {
  const ssh = fieldsForProvider(provider('generic', 'ssh'))

  it('asks for the public key when only the private key is filled in', () => {
    const errors = credentialFieldErrors(ssh, { private_key: '-----BEGIN…' })
    expect(Object.keys(errors)).toEqual(['public_key'])
    expect(errors.public_key).toContain('replaced together')
  })

  it('asks for the private key when only the public key is filled in', () => {
    const errors = credentialFieldErrors(ssh, {
      public_key: 'ssh-ed25519 AAAA',
    })
    expect(Object.keys(errors)).toEqual(['private_key'])
  })

  it('accepts both keys, or neither', () => {
    expect(
      credentialFieldErrors(ssh, {
        private_key: '-----BEGIN…',
        public_key: 'ssh-ed25519 AAAA',
      })
    ).toEqual({})
    expect(credentialFieldErrors(ssh, { public_key: '   ' })).toEqual({})
  })

  it('leaves fields without a partner alone', () => {
    const basic = fieldsForProvider(provider('generic', 'basic'))
    expect(credentialFieldErrors(basic, { password: 'new' })).toEqual({})
  })
})
