// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  generateDependentServiceTemplateValue,
  generateServiceTemplateValue,
  serviceTemplateVariableIsGenerated,
} from './service-template-values'

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

  test('only user inputs require manual entry', () => {
    expect(serviceTemplateVariableIsGenerated('public_url')).toBe(true)
    expect(serviceTemplateVariableIsGenerated('generated_password')).toBe(true)
    expect(serviceTemplateVariableIsGenerated('user_input')).toBe(false)
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
  })
})
