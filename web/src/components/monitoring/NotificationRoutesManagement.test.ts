// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { NotificationProviderResponse } from '@/api/client/types.gen'
import { describe, expect, it } from 'bun:test'
import {
  configuredSlackChannel,
  severityRangeLabel,
} from './notificationRouteUtils'

const provider = (
  providerType: string,
  config: unknown
): NotificationProviderResponse => ({
  id: 1,
  name: 'Destination',
  provider_type: providerType,
  config,
  enabled: true,
  created_at: 0,
  updated_at: 0,
})

describe('notification route presentation', () => {
  it('describes default, exact, bounded, and threshold severity ranges', () => {
    expect(severityRangeLabel('debug', 'emergency')).toBe('All severities')
    expect(severityRangeLabel('warning', 'warning')).toBe('Warning only')
    expect(severityRangeLabel('warning', 'error')).toBe('Warning through Error')
    expect(severityRangeLabel('critical', 'emergency')).toBe(
      'Critical and above'
    )
  })

  it('uses a configured Slack channel only for Slack providers', () => {
    expect(
      configuredSlackChannel(provider('slack', { channel: ' #ops ' }))
    ).toBe('#ops')
    expect(configuredSlackChannel(provider('email', { channel: '#ops' }))).toBe(
      undefined
    )
    expect(configuredSlackChannel(provider('slack', { channel: null }))).toBe(
      undefined
    )
  })
})
