// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { CloudflareProjectCapability } from '@/api/client'
import { dropDeliveryChoices } from './drop-delivery'

const unconfigured: CloudflareProjectCapability = {
  configured: false,
  default_enabled: false,
  bunny_configured: false,
  bunny_default_enabled: false,
  reason: 'Create a Cloudflare delivery profile',
  bunny_reason: 'Create a Bunny delivery profile with an active Pull Zone',
}

describe('dropDeliveryChoices', () => {
  test('offers only No CDN while the capability is loading', () => {
    expect(dropDeliveryChoices(undefined)).toEqual(['none'])
  })

  test('offers only No CDN when no provider is set up', () => {
    expect(dropDeliveryChoices(unconfigured)).toEqual(['none'])
  })

  test('offers each configured provider', () => {
    expect(
      dropDeliveryChoices({
        ...unconfigured,
        configured: true,
        bunny_configured: true,
      })
    ).toEqual(['none', 'cloudflare', 'bunny'])
    expect(
      dropDeliveryChoices({ ...unconfigured, bunny_configured: true })
    ).toEqual(['none', 'bunny'])
  })

  test('keeps the server default selectable', () => {
    expect(
      dropDeliveryChoices({ ...unconfigured, default_enabled: true })
    ).toEqual(['none', 'cloudflare'])
  })
})
