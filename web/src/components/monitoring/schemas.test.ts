// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  notificationRouteSeveritySchema,
  providerSchema,
  providerUpdateSchema,
  telegramConfigFromForm,
  telegramFormFieldsFromConfig,
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

describe('telegram provider form', () => {
  const telegram = {
    name: 'Phone alerts',
    provider_type: 'telegram' as const,
    config: {
      bot_token: '123456789:AAExampleSecretToken',
      chat_id: '-1001234567890',
    },
  }

  const issuePaths = (input: unknown, schema = providerSchema) => {
    const result = schema.safeParse(input)
    return result.success
      ? []
      : result.error.issues.map((issue) => issue.path.join('.'))
  }

  test('accepts numeric and @username chats with an optional topic', () => {
    expect(providerSchema.safeParse(telegram).success).toBe(true)
    expect(
      providerSchema.safeParse({
        ...telegram,
        config: { ...telegram.config, chat_id: '@ops_channel' },
      }).success
    ).toBe(true)
    const withTopic = providerSchema.safeParse({
      ...telegram,
      config: { ...telegram.config, message_thread_id: 42 },
    })
    expect(withTopic.success).toBe(true)
  })

  test('a provider stored with an integer chat id (generic API) can be edited', () => {
    // POST /notification-providers accepts `chat_id` as a JSON integer and
    // stores the config as submitted; the API then returns it masked.
    const stored = {
      bot_token: '***',
      chat_id: -1001234567890,
      message_thread_id: 7,
    }
    const fields = telegramFormFieldsFromConfig(stored)
    expect(fields).toEqual({
      bot_token: '***',
      chat_id: '-1001234567890',
      message_thread_id: 7,
    })
    // A rename-only save from the edit page must validate.
    const result = providerUpdateSchema.safeParse({
      ...telegram,
      name: 'Renamed alerts',
      config: fields,
    })
    expect(result.success).toBe(true)
    if (result.success) {
      expect(telegramConfigFromForm(result.data.config).chat_id).toBe(
        '-1001234567890'
      )
    }
  })

  test('missing or malformed stored Telegram fields load as empty, not as casts', () => {
    expect(telegramFormFieldsFromConfig({})).toEqual({
      bot_token: '',
      chat_id: '',
      message_thread_id: undefined,
    })
    expect(
      telegramFormFieldsFromConfig({ chat_id: null, message_thread_id: '42' })
    ).toEqual({ bot_token: '', chat_id: '', message_thread_id: undefined })
  })

  test('an emptied topic (NaN) is treated as unset', () => {
    const result = providerSchema.safeParse({
      ...telegram,
      config: { ...telegram.config, message_thread_id: Number.NaN },
    })
    expect(result.success).toBe(true)
    if (result.success) {
      expect(
        telegramConfigFromForm(result.data.config).message_thread_id
      ).toBeNull()
    }
  })

  test('points at the bot token field when it is missing or malformed', () => {
    expect(
      issuePaths({ ...telegram, config: { ...telegram.config, bot_token: '' } })
    ).toEqual(['config.bot_token'])
    expect(
      issuePaths({
        ...telegram,
        config: { ...telegram.config, bot_token: 'not-a-token' },
      })
    ).toEqual(['config.bot_token'])
  })

  test('points at the chat field when it is not a chat id', () => {
    expect(
      issuePaths({
        ...telegram,
        config: { ...telegram.config, chat_id: 'my group' },
      })
    ).toEqual(['config.chat_id'])
  })

  test('rejects a zero or negative topic id', () => {
    expect(
      issuePaths({
        ...telegram,
        config: { ...telegram.config, message_thread_id: 0 },
      })
    ).toEqual(['config.message_thread_id'])
  })

  test('the masked token is only accepted when editing', () => {
    const masked = {
      ...telegram,
      config: { ...telegram.config, bot_token: '***' },
    }
    expect(providerUpdateSchema.safeParse(masked).success).toBe(true)
    expect(providerSchema.safeParse(masked).success).toBe(false)
  })

  test('builds the stored config with trimmed values', () => {
    expect(
      telegramConfigFromForm({
        bot_token: ' 123:abc ',
        chat_id: ' -100 ',
        message_thread_id: 7,
      })
    ).toEqual({ bot_token: '123:abc', chat_id: '-100', message_thread_id: 7 })
  })
})
