// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isLikelySecretProjectEnvironmentVariable,
  projectEnvironmentVariablesSchema,
} from './project-environment-variables'

describe('projectEnvironmentVariablesSchema', () => {
  test('accepts plain values and non-empty secrets', () => {
    const result = projectEnvironmentVariablesSchema.safeParse([
      { key: 'NODE_ENV', value: 'production', isSecret: false },
      { key: 'API_TOKEN', value: 'secret-value', isSecret: true },
    ])

    expect(result.success).toBe(true)
  })

  test('trims variable names before submission', () => {
    const result = projectEnvironmentVariablesSchema.parse([
      { key: '  DATABASE_URL  ', value: 'postgres://db', isSecret: true },
    ])

    expect(result[0]?.key).toBe('DATABASE_URL')
  })

  test.each(['', '2FA_TOKEN', 'API-KEY', 'HAS SPACE'])(
    'rejects invalid variable name %p',
    (key) => {
      const result = projectEnvironmentVariablesSchema.safeParse([
        { key, value: 'value', isSecret: false },
      ])

      expect(result.success).toBe(false)
    }
  )

  test('rejects duplicate variable names at the duplicate row', () => {
    const result = projectEnvironmentVariablesSchema.safeParse([
      { key: 'DATABASE_URL', value: 'first', isSecret: false },
      { key: 'DATABASE_URL', value: 'second', isSecret: false },
    ])

    expect(result.success).toBe(false)
    if (result.success) return
    expect(result.error.issues).toContainEqual(
      expect.objectContaining({
        message: 'DATABASE_URL is already defined',
        path: [1, 'key'],
      })
    )
  })

  test('rejects an empty write-only secret', () => {
    const result = projectEnvironmentVariablesSchema.safeParse([
      { key: 'API_TOKEN', value: '', isSecret: true },
    ])

    expect(result.success).toBe(false)
    if (result.success) return
    expect(result.error.issues).toContainEqual(
      expect.objectContaining({
        message: 'A secret needs a value — it cannot be filled in later',
        path: [0, 'value'],
      })
    )
  })

  test.each([
    'SENTRY_DSN',
    'NEXT_PUBLIC_SENTRY_DSN',
    'OTEL_EXPORTER_OTLP_ENDPOINT',
    'SENTRY_RELEASE',
    'OTEL_SERVICE_VERSION',
    'HOST',
  ])(
    'leaves backend-owned policy decisions to the managed catalog for %s',
    (key) => {
      const result = projectEnvironmentVariablesSchema.safeParse([
        { key, value: 'user-value', isSecret: false },
      ])

      expect(result.success).toBe(true)
    }
  )
})

describe('isLikelySecretProjectEnvironmentVariable', () => {
  test.each(['API_TOKEN', 'DATABASE_URL', 'NEXTAUTH_SECRET', 'SENTRY_DSN'])(
    'marks %s as a likely secret',
    (key) => {
      expect(isLikelySecretProjectEnvironmentVariable(key)).toBe(true)
    }
  )

  test.each(['NODE_ENV', 'NEXTAUTH_URL', 'PUBLIC_API_URL', 'PORT'])(
    'keeps %s readable by default',
    (key) => {
      expect(isLikelySecretProjectEnvironmentVariable(key)).toBe(false)
    }
  )
})
