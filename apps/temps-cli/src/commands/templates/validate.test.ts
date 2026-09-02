// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { validateNativeTemplateConfig } from './validate.js'

describe('validateNativeTemplateConfig', () => {
  test('accepts a pinned PostgreSQL-backed service', () => {
    const result = validateNativeTemplateConfig({
      version: '2',
      templates: [
        {
          slug: 'keycloak',
          name: 'Keycloak',
          kind: 'service',
          image: 'quay.io/keycloak/keycloak:26.7.2',
          exposed_port: 8080,
          resources: {
            cpu_request: 500000,
            memory_request: 512,
            memory_limit: 1536,
          },
          services: ['postgres'],
          managed_service_bindings: {
            postgres: { KC_DB_USERNAME: 'POSTGRES_USER' },
          },
        },
      ],
    })

    expect(result).toEqual({ valid: true, errors: [], templateCount: 1 })
  })

  test('rejects floating images and undeclared bindings', () => {
    const result = validateNativeTemplateConfig({
      version: '2',
      templates: [
        {
          slug: 'bad-service',
          name: 'Bad service',
          kind: 'service',
          image: 'example/app:latest',
          exposed_port: 3000,
          services: [],
          managed_service_bindings: {
            postgres: { DATABASE_URL: 'POSTGRES_URL' },
          },
        },
      ],
    })

    expect(result.valid).toBeFalse()
    expect(result.errors).toContain(
      'templates[0].image must not use the floating latest tag'
    )
    expect(result.errors).toContain(
      'templates[0].managed_service_bindings.postgres must also be listed in services'
    )
  })
})
