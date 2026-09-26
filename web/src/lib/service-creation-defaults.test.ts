// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { serviceCreationDefaults } from './service-creation-defaults'

describe('serviceCreationDefaults', () => {
  test('reads the backend-owned name and parameter defaults', () => {
    expect(
      serviceCreationDefaults({
        'x-temps-creation-defaults': {
          name: 'redis-a1b2',
          parameters: {
            port: 6379,
            docker_image: 'gotempsh/redis-walg:8-bookworm',
          },
          topology: 'standalone',
          node_id: null,
        },
      })
    ).toEqual({
      name: 'redis-a1b2',
      parameters: {
        port: 6379,
        docker_image: 'gotempsh/redis-walg:8-bookworm',
      },
      topology: 'standalone',
      node_id: null,
    })
  })

  test('fails closed when the extension is malformed', () => {
    expect(
      serviceCreationDefaults({
        'x-temps-creation-defaults': { name: 'redis' },
      })
    ).toBeNull()
  })
})
