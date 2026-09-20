// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  providerButtonLabel,
  type OidcProviderOption,
} from '@/components/auth/login-form'

describe('providerButtonLabel', () => {
  test('reads "Continue with Temps Cloud" for the Cloud-managed provider', () => {
    const provider: OidcProviderOption = {
      slug: 'temps-cloud-abcd',
      name: 'Temps Cloud',
      template: 'temps_cloud',
    }
    expect(providerButtonLabel(provider)).toBe('Continue with Temps Cloud')
  })

  test('reads "Sign in with <name>" for an ordinary SSO provider', () => {
    const provider: OidcProviderOption = {
      slug: 'okta-1234',
      name: 'Okta',
      template: 'okta',
    }
    expect(providerButtonLabel(provider)).toBe('Sign in with Okta')
  })

  test('falls back to "Sign in with <name>" when no template is set', () => {
    const provider: OidcProviderOption = {
      slug: 'generic-9999',
      name: 'Corporate SSO',
    }
    expect(providerButtonLabel(provider)).toBe('Sign in with Corporate SSO')
  })
})
