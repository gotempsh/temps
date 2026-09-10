// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { normalizeEnvironmentsResponse, validateHealthCheckPath } from './deploy-image.js'
import type { EnvironmentResponse } from '../../api/types.gen.js'

function makeEnvironment(overrides: Partial<EnvironmentResponse> = {}): EnvironmentResponse {
  return {
    id: 1,
    name: 'production',
    slug: 'production',
    subdomain: 'app-production',
    main_url: 'https://app-production.example.com',
    project_id: 1,
    is_preview: false,
    protected: false,
    sleeping: false,
    created_at: 1_700_000_000,
    updated_at: 1_700_000_000,
    ...overrides,
  }
}

describe('normalizeEnvironmentsResponse', () => {
  test('returns a plain array untouched', () => {
    const envs = [makeEnvironment()]
    expect(normalizeEnvironmentsResponse(envs)).toBe(envs)
  })

  test('unwraps a { data: [...] } wrapper', () => {
    const envs = [makeEnvironment()]
    expect(normalizeEnvironmentsResponse({ data: envs })).toEqual(envs)
  })

  test('unwraps a { environments: [...] } wrapper', () => {
    const envs = [makeEnvironment()]
    expect(normalizeEnvironmentsResponse({ environments: envs })).toEqual(envs)
  })

  test('returns an empty array for null, undefined, or an unrecognized shape', () => {
    // Silently returning [] (rather than throwing) here is intentional — the
    // caller falls back to "no environments found", which is a legible error
    // rather than a crash.
    expect(normalizeEnvironmentsResponse(undefined)).toEqual([])
    expect(normalizeEnvironmentsResponse(null)).toEqual([])
    expect(normalizeEnvironmentsResponse({ foo: 'bar' })).toEqual([])
  })
})

describe('validateHealthCheckPath', () => {
  test('trims and returns a path that starts with /', () => {
    expect(validateHealthCheckPath('  /api/healthz  ')).toBe('/api/healthz')
  })

  test('rejects a relative path so it cannot silently misconfigure the uptime monitor', () => {
    expect(() => validateHealthCheckPath('api/healthz')).toThrow(/must start with "\/"/)
  })
})
