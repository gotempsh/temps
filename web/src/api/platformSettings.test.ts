// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildPlatformSettingsUpdateBody } from './platformSettings'
import type { PlatformSettings } from './platformSettings'

describe('buildPlatformSettingsUpdateBody', () => {
  test('includes Docker registry configuration in the settings request', () => {
    const dockerRegistry = {
      enabled: true,
      registry_url: 'https://registry.example.test',
      username: 'registry-user',
      password: 'registry-token',
      tls_verify: true,
      ca_certificate: null,
    }

    const body = buildPlatformSettingsUpdateBody({
      docker_registry: dockerRegistry,
    } as PlatformSettings)

    expect(body.docker_registry).toEqual(dockerRegistry)
  })
})
