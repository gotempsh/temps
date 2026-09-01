// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { discoverComposeEnvironmentVariables } from './compose-environment-discovery'

describe('discoverComposeEnvironmentVariables', () => {
  test('shows missing env-example and per-service keys with deduplicated sources', () => {
    expect(
      discoverComposeEnvironmentVariables({
        configuredKeys: ['DATABASE_URL'],
        envExamplePath: '.env.example',
        envExampleVariables: [
          { key: 'DATABASE_URL' },
          { key: 'APP_SECRET', description: 'Application signing key' },
        ],
        composePath: 'compose.yml',
        composeServices: [
          {
            name: 'web',
            environmentVariables: ['APP_SECRET', 'DATABASE_URL'],
          },
          { name: 'worker', environmentVariables: ['APP_SECRET', 'QUEUE_URL'] },
        ],
      })
    ).toEqual([
      {
        key: 'APP_SECRET',
        description: 'Application signing key',
        sources: ['.env.example', 'compose.yml · web', 'compose.yml · worker'],
      },
      {
        key: 'QUEUE_URL',
        description: undefined,
        sources: ['compose.yml · worker'],
      },
    ])
  })
})
