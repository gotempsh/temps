// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  confirmedServiceTemplateCapabilities,
  createServiceTemplateWithSlugRetry,
  generateDependentServiceTemplateValue,
  generateServiceTemplateValue,
  serviceTemplateVariableIsGenerated,
} from './service-template-values'

describe('confirmedServiceTemplateCapabilities', () => {
  const requirements = [
    { service: 'postgres' },
    { service: 'redis' },
    { service: 'postgres' },
  ]

  test('confirms every required service with one stack-level choice', () => {
    expect(confirmedServiceTemplateCapabilities(requirements, true)).toEqual([
      'postgres',
      'redis',
    ])
  })

  test('does not grant startup capabilities before confirmation', () => {
    expect(confirmedServiceTemplateCapabilities(requirements, false)).toEqual(
      []
    )
  })
})

const base = { scheme: 'https' as const, host: 'apps.example.com' }

describe('service template value generation', () => {
  test('uses the same production hostname as Temps deployments', () => {
    expect(
      generateServiceTemplateValue(
        { name: 'SERVICE_URL_APP_3000', kind: 'public_url' },
        'actual-budget',
        base
      )
    ).toBe('https://actual-budget-production.apps.example.com')
    expect(
      generateServiceTemplateValue(
        { name: 'SERVICE_FQDN_APP', kind: 'public_host' },
        'actual-budget',
        base
      )
    ).toBe('actual-budget-production.apps.example.com')
  })

  test('honors upstream defaults before generators', () => {
    expect(
      generateServiceTemplateValue(
        { name: 'LOG_LEVEL', kind: 'user_input', defaultValue: 'info' },
        'example',
        base
      )
    ).toBe('info')
    expect(
      generateServiceTemplateValue(
        {
          name: 'SERVICE_URL_APP_3000',
          kind: 'public_url',
          defaultValue: '/console',
        },
        'example',
        base
      )
    ).toBe('https://example-production.apps.example.com/console')
    expect(
      generateServiceTemplateValue(
        {
          name: 'SERVICE_PASSWORD_DOCUMENSO',
          kind: 'generated_password',
          defaultValue: '',
        },
        'example',
        base
      )
    ).toHaveLength(32)
  })

  test('uses a stable per-service hostname after the primary route', () => {
    expect(
      generateServiceTemplateValue(
        {
          name: 'SERVICE_URL_ADMIN_3001',
          kind: 'public_url',
          routeService: 'admin',
          routeIsPrimary: false,
        },
        'example',
        base
      )
    ).toBe('https://admin--example-production.apps.example.com')
  })

  test('generates values with the requested lengths', () => {
    expect(
      generateServiceTemplateValue(
        { name: 'SERVICE_PASSWORD_64_APP', kind: 'generated_password_64' },
        'example',
        base
      )
    ).toHaveLength(64)
    expect(
      generateServiceTemplateValue(
        {
          name: 'SERVICE_PASSWORDWITHSYMBOLS_64_APP',
          kind: 'generated_password_with_symbols_64',
        },
        'example',
        base
      )
    ).toHaveLength(64)
    expect(
      generateServiceTemplateValue(
        { name: 'SERVICE_HEX_64_APP', kind: 'generated_hex_64' },
        'example',
        base
      )
    ).toHaveLength(64)
  })

  test('uses conventional service usernames instead of random identifiers', () => {
    expect(
      generateServiceTemplateValue(
        { name: 'SERVICE_USER_POSTGRES', kind: 'generated_user' },
        'example',
        base
      )
    ).toBe('postgres')
    expect(
      generateServiceTemplateValue(
        { name: 'SERVICE_USER_REDIS', kind: 'generated_user' },
        'example',
        base
      )
    ).toBe('default')
    expect(
      generateServiceTemplateValue(
        { name: 'SERVICE_USER_MINIO', kind: 'generated_user' },
        'example',
        base
      )
    ).toBe('admin')
    expect(
      generateServiceTemplateValue(
        {
          name: 'SERVICE_USER_POSTGRES',
          kind: 'generated_user',
          defaultValue: 'app_owner',
        },
        'example',
        base
      )
    ).toBe('app_owner')
  })

  test('only user inputs require manual entry', () => {
    expect(serviceTemplateVariableIsGenerated('public_url')).toBe(true)
    expect(serviceTemplateVariableIsGenerated('generated_password')).toBe(true)
    expect(serviceTemplateVariableIsGenerated('user_input')).toBe(false)
    expect(serviceTemplateVariableIsGenerated('future_generator')).toBe(false)
  })

  test('generates dependent Supabase JWT roles from the shared signing key', async () => {
    const values = { SERVICE_PASSWORD_JWT: 'a-secure-signing-key' }
    const anon = await generateDependentServiceTemplateValue(
      'generated_supabase_anon',
      values
    )
    const service = await generateDependentServiceTemplateValue(
      'generated_supabase_service',
      values
    )
    const decodeRole = (token: string | null) => {
      const payload = token?.split('.')[1] || ''
      const padded = payload.replace(/-/g, '+').replace(/_/g, '/')
      return JSON.parse(atob(padded)).role
    }
    expect(decodeRole(anon)).toBe('anon')
    expect(decodeRole(service)).toBe('service_role')

    const verify = async (token: string | null) => {
      const [header, payload, signature = ''] = token?.split('.') || []
      const padded = signature
        .replace(/-/g, '+')
        .replace(/_/g, '/')
        .padEnd(Math.ceil(signature.length / 4) * 4, '=')
      const bytes = Uint8Array.from(atob(padded), (character) =>
        character.charCodeAt(0)
      )
      const key = await crypto.subtle.importKey(
        'raw',
        new TextEncoder().encode(values.SERVICE_PASSWORD_JWT),
        { name: 'HMAC', hash: 'SHA-256' },
        false,
        ['verify']
      )
      return crypto.subtle.verify(
        'HMAC',
        key,
        bytes,
        new TextEncoder().encode(`${header}.${payload}`)
      )
    }
    expect(await verify(anon)).toBe(true)
    expect(await verify(service)).toBe(true)
  })
})

describe('service template slug conflict recovery', () => {
  test('re-plans once after an explicit slug conflict', async () => {
    const attempts: string[] = []
    const result = await createServiceTemplateWithSlugRetry(
      'actualbudget',
      async (slug) => {
        attempts.push(slug)
        if (slug === 'actualbudget') throw { status: 409 }
        return `created:${slug}`
      },
      async () => 'actualbudget-a1b2c3',
      (error) =>
        typeof error === 'object' &&
        error !== null &&
        'status' in error &&
        error.status === 409
    )

    expect(attempts).toEqual(['actualbudget', 'actualbudget-a1b2c3'])
    expect(result).toEqual({
      plan: 'actualbudget-a1b2c3',
      result: 'created:actualbudget-a1b2c3',
    })
  })

  test('does not retry ambiguous or non-conflict failures', async () => {
    let replans = 0
    const failure = new Error('network response was lost')

    await expect(
      createServiceTemplateWithSlugRetry(
        'actualbudget',
        async () => {
          throw failure
        },
        async () => {
          replans += 1
          return 'should-not-run'
        },
        (error) =>
          typeof error === 'object' &&
          error !== null &&
          'status' in error &&
          error.status === 409
      )
    ).rejects.toBe(failure)
    expect(replans).toBe(0)
  })
})
