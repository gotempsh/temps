// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  notificationRouteSeveritySchema,
  providerSchema,
  providerUpdateSchema,
} from './schemas'

const slackProvider = {
  name: 'On-call Slack',
  provider_type: 'slack' as const,
  config: {
    webhook_url: 'https://hooks.slack.com/services/example',
    channel: '#on-call',
  },
}

describe('notification routing schemas', () => {
  test('keeps delivery details valid without routing fields on providers', () => {
    expect(providerSchema.safeParse(slackProvider).success).toBe(true)
    expect(providerUpdateSchema.safeParse(slackProvider).success).toBe(true)
  })

  test('accepts every supported route severity', () => {
    for (const severity of [
      'debug',
      'info',
      'warning',
      'error',
      'critical',
      'emergency',
    ]) {
      expect(notificationRouteSeveritySchema.safeParse(severity).success).toBe(
        true
      )
    }
  })

  test('rejects unsupported route severities', () => {
    expect(notificationRouteSeveritySchema.safeParse('verbose').success).toBe(
      false
    )
  })
})
