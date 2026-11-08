// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  accessServiceSchema,
  serviceDefaults,
  serviceBody,
  grantRequestSchema,
  grantLabels,
} from './service-access-form'
import type { AccessServiceResponse } from '@/api/client'

const service: AccessServiceResponse = {
  id: 'svc_test',
  project_id: 7,
  name: 'API',
  policy: {
    origin: 'https://api.example.com',
    path_prefixes: ['/v1'],
    methods: ['GET'],
  },
  enabled: true,
  has_credential: true,
  max_ttl_seconds: 900,
  version: 4,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
test('editing keeps credentials absent and carries the saved version', () => {
  const defaults = serviceDefaults(service)
  expect(defaults.credential).toBe('')
  expect(serviceBody(defaults, service.version)).toEqual({
    name: 'API',
    policy: service.policy,
    enabled: true,
    max_ttl_seconds: 900,
    expected_version: 4,
    credential: undefined,
    clear_credential: false,
  })
})
test('a credential can be replaced or cleared but never both', () => {
  const values = { ...serviceDefaults(service), credential: 'test-bearer' }
  expect(serviceBody(values).credential).toBe('test-bearer')
  expect(
    accessServiceSchema.safeParse({ ...values, clearCredential: true }).success
  ).toBe(false)
  expect(
    serviceBody({ ...values, credential: '', clearCredential: true })
      .clear_credential
  ).toBe(true)
})
test('validates HTTPS origins, names, methods and lifetime bounds', () => {
  expect(accessServiceSchema.safeParse(serviceDefaults(service)).success).toBe(
    true
  )
  for (const origin of [
    'invalid-url',
    'http://api.example.com',
    'https://user:password@api.example.com',
    'https://api.example.com/path',
    'https://api.example.com?token=example',
  ]) {
    expect(
      accessServiceSchema.safeParse({ ...serviceDefaults(service), origin })
        .success
    ).toBe(false)
  }
  for (const maxTtl of [0, 59, 86401]) {
    expect(
      accessServiceSchema.safeParse({ ...serviceDefaults(service), maxTtl })
        .success
    ).toBe(false)
  }
  expect(
    accessServiceSchema.safeParse({ ...serviceDefaults(service), name: ' ' })
      .success
  ).toBe(false)
  expect(
    accessServiceSchema.safeParse({ ...serviceDefaults(service), methods: [] })
      .success
  ).toBe(false)
})
test('request lifetimes include boundaries and reject fractions', () => {
  for (const ttl of [60, 86400])
    expect(grantRequestSchema.safeParse({ ttl }).success).toBe(true)
  for (const ttl of [0, 59, 900.5, 86401])
    expect(grantRequestSchema.safeParse({ ttl }).success).toBe(false)
})
test('all current grant statuses avoid claiming a connection', () => {
  expect(grantLabels.integration_unavailable).toBe('Not connected')
  expect(Object.values(grantLabels)).not.toContain('Connected')
})
