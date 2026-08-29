// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ManagedEnvironmentVariable } from '@/api/client/types.gen'
import {
  databaseProvidedEnvironmentVariable,
  findProvidedEnvironmentVariableCollision,
  groupManagedEnvironmentVariables,
  normalizeCreationPreset,
} from '@/lib/provided-environment-variables'

describe('ProvidedEnvironmentVariables', () => {
  test('normalizes repository presets and keeps only the preset slug', () => {
    expect(normalizeCreationPreset('NextJS::apps/web')).toBe('nextjs')
    expect(normalizeCreationPreset('')).toBe('dockerfile')
  })

  test('groups backend variables in a stable user-facing order', () => {
    const variables: ManagedEnvironmentVariable[] = [
      {
        name: 'TEMPS_API_URL',
        source: 'temps',
        is_secret: false,
        is_user_overridable: false,
        description: 'Temps API',
      },
      {
        name: 'OTEL_EXPORTER_OTLP_HEADERS',
        source: 'open_telemetry',
        is_secret: true,
        is_user_overridable: false,
        description: 'OTLP headers',
      },
      {
        name: 'SENTRY_DSN',
        source: 'error_tracking',
        is_secret: false,
        is_user_overridable: false,
        description: 'Error tracking DSN',
      },
    ]

    expect(
      groupManagedEnvironmentVariables(variables).map((group) => group.source)
    ).toEqual(['error_tracking', 'open_telemetry', 'temps'])
  })

  test('detects exact platform and selected-database collisions', () => {
    const providedVariables = [
      {
        name: 'SENTRY_TUNNEL',
        provider: 'Temps',
        isUserOverridable: false,
      },
      {
        name: 'POSTGRES_URL',
        provider: 'database "app-db"',
        isUserOverridable: false,
      },
    ]

    expect(
      findProvidedEnvironmentVariableCollision(
        ' POSTGRES_URL ',
        providedVariables
      )?.provider
    ).toBe('database "app-db"')
    expect(
      findProvidedEnvironmentVariableCollision(
        'POSTGRES_URL_BACKUP',
        providedVariables
      )
    ).toBeUndefined()
  })

  test('treats linked database variables as user-overridable defaults', () => {
    expect(
      databaseProvidedEnvironmentVariable('POSTGRES_URL', 'app-db')
    ).toEqual({
      name: 'POSTGRES_URL',
      provider: 'database "app-db"',
      isUserOverridable: true,
    })
  })
})
