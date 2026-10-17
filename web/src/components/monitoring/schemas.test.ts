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

describe('provider numeric fields that were emptied', () => {
  const email = {
    name: 'Ops mail',
    provider_type: 'email' as const,
    config: {
      smtp_host: 'smtp.example.test',
      smtp_port: 587,
      from_address: 'alerts@example.test',
      to_addresses: ['ops@example.test'],
    },
  }
  const webhook = {
    name: 'Hook',
    provider_type: 'webhook' as const,
    config: { url: 'https://hooks.example.test/in', timeout_secs: 30 },
  }

  test('an emptied SMTP port (NaN) fails the email validation instead of passing', () => {
    const result = providerSchema.safeParse({
      ...email,
      config: { ...email.config, smtp_port: Number.NaN },
    })
    expect(result.success).toBe(false)
  })

  test('an out-of-range SMTP port is still rejected', () => {
    const result = providerSchema.safeParse({
      ...email,
      config: { ...email.config, smtp_port: 70000 },
    })
    expect(result.success).toBe(false)
  })

  test('an emptied timeout (NaN) means "use the default" and is accepted as unset', () => {
    const result = providerSchema.safeParse({
      ...webhook,
      config: { ...webhook.config, timeout_secs: Number.NaN },
    })
    expect(result.success).toBe(true)
    if (result.success) expect(result.data.config.timeout_secs).toBeUndefined()
  })

  test('an out-of-range timeout is still rejected', () => {
    const result = providerSchema.safeParse({
      ...webhook,
      config: { ...webhook.config, timeout_secs: 301 },
    })
    expect(result.success).toBe(false)
  })
})
