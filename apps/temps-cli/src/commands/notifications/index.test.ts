// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  parseEnabledFlag,
  hasSlackConfigChanges,
  hasEmailConfigChanges,
  hasWebhookConfigChanges,
  buildSlackConfigUpdate,
  buildEmailConfigUpdate,
  buildWebhookConfigUpdate,
  collectHeaderFlag,
  parseHeaderFlags,
  withHeaderHint,
  parseProviderIds,
} from './index.js'

describe('parseEnabledFlag', () => {
  test('leaves the flag untouched when not provided', () => {
    expect(parseEnabledFlag(undefined)).toBeUndefined()
  })

  test('parses the literal strings "true" and "false"', () => {
    expect(parseEnabledFlag('true')).toBe(true)
    expect(parseEnabledFlag('false')).toBe(false)
  })

  test('flags anything else as invalid instead of silently coercing it', () => {
    expect(parseEnabledFlag('yes')).toBe('invalid')
    expect(parseEnabledFlag('1')).toBe('invalid')
  })
})

describe('hasSlackConfigChanges / hasEmailConfigChanges / hasWebhookConfigChanges', () => {
  test('slack: true only when webhook-url or channel is present', () => {
    expect(hasSlackConfigChanges({})).toBe(false)
    expect(hasSlackConfigChanges({ webhookUrl: 'https://hooks.slack.com/x' })).toBe(true)
    expect(hasSlackConfigChanges({ channel: '#alerts' })).toBe(true)
  })

  test('email: true when any SMTP field is present', () => {
    expect(hasEmailConfigChanges({})).toBe(false)
    expect(hasEmailConfigChanges({ smtpHost: 'smtp.example.com' })).toBe(true)
    expect(hasEmailConfigChanges({ toAddresses: 'a@example.com' })).toBe(true)
  })

  test('webhook: true only when url or method is present', () => {
    expect(hasWebhookConfigChanges({})).toBe(false)
    expect(hasWebhookConfigChanges({ url: 'https://example.com/hook' })).toBe(true)
    expect(hasWebhookConfigChanges({ method: 'PUT' })).toBe(true)
    expect(hasWebhookConfigChanges({ header: ['X-Foo: bar'] })).toBe(true)
  })

  test('name/enabled-only updates are correctly detected as "no type-specific config"', () => {
    // This is what routes `notifications update --id 1 --enabled false` to
    // the generic update instead of a type-specific one that would otherwise
    // require (and default) fields the caller never asked to change.
    expect(hasSlackConfigChanges({})).toBe(false)
    expect(hasEmailConfigChanges({})).toBe(false)
    expect(hasWebhookConfigChanges({})).toBe(false)
  })
})

describe('buildSlackConfigUpdate', () => {
  test('uses supplied values when present', () => {
    expect(buildSlackConfigUpdate(
      { webhookUrl: 'https://hooks.slack.com/new', channel: '#new-channel' },
      { webhook_url: 'https://hooks.slack.com/old', channel: '#old-channel' },
    )).toEqual({ webhook_url: 'https://hooks.slack.com/new', channel: '#new-channel' })
  })

  test('falls back to the current config for fields not supplied', () => {
    // A partial update (only --name or --enabled) must not wipe out the
    // webhook URL that was already configured.
    expect(buildSlackConfigUpdate(
      {},
      { webhook_url: 'https://hooks.slack.com/old', channel: '#old-channel' },
    )).toEqual({ webhook_url: 'https://hooks.slack.com/old', channel: '#old-channel' })
  })

  test('clearing the channel with an empty string sets it to null rather than keeping the old value', () => {
    expect(buildSlackConfigUpdate(
      { channel: '' },
      { webhook_url: 'https://hooks.slack.com/old', channel: '#old-channel' },
    )).toEqual({ webhook_url: 'https://hooks.slack.com/old', channel: null })
  })

  test('defaults to empty/null when there is no current config at all', () => {
    expect(buildSlackConfigUpdate({}, null)).toEqual({ webhook_url: '', channel: null })
  })
})

describe('buildEmailConfigUpdate', () => {
  const current = {
    smtp_host: 'smtp.old.com',
    smtp_port: 25,
    username: 'old-user',
    password: 'old-pass',
    from_address: 'old@example.com',
    from_name: 'Old Name',
    to_addresses: ['a@example.com'],
  }

  test('merges supplied fields over the current config', () => {
    expect(buildEmailConfigUpdate({ smtpHost: 'smtp.new.com', toAddresses: 'b@example.com, c@example.com' }, current)).toEqual({
      smtp_host: 'smtp.new.com',
      smtp_port: 25,
      username: 'old-user',
      password: 'old-pass',
      from_address: 'old@example.com',
      from_name: 'Old Name',
      to_addresses: ['b@example.com', 'c@example.com'],
    })
  })

  test('parses smtp-port from a string and preserves the current port when omitted', () => {
    expect(buildEmailConfigUpdate({ smtpPort: '2525' }, current).smtp_port).toBe(2525)
    expect(buildEmailConfigUpdate({}, current).smtp_port).toBe(25)
  })

  test('defaults smtp-port to 587 when there is no current config', () => {
    expect(buildEmailConfigUpdate({}, null).smtp_port).toBe(587)
  })

  test('clearing from-name with an empty string sets it to null', () => {
    expect(buildEmailConfigUpdate({ fromName: '' }, current).from_name).toBeNull()
  })
})

describe('buildWebhookConfigUpdate', () => {
  test('merges supplied fields over the current config', () => {
    expect(buildWebhookConfigUpdate(
      { method: 'PUT' },
      { url: 'https://example.com/hook', method: 'POST', headers: { 'X-Foo': 'bar' }, timeout_secs: 15 },
    )).toEqual({
      url: 'https://example.com/hook',
      method: 'PUT',
      headers: { 'X-Foo': 'bar' },
      timeout_secs: 15,
    })
  })

  test('defaults method to POST and timeout to 30s when there is no current config', () => {
    expect(buildWebhookConfigUpdate({ url: 'https://example.com/hook' }, null)).toEqual({
      url: 'https://example.com/hook',
      method: 'POST',
      headers: {},
      timeout_secs: 30,
    })
  })

  test('supplied headers replace the stored ones', () => {
    expect(buildWebhookConfigUpdate(
      { url: 'https://other.example.net/hook', headers: { Authorization: 'Bearer new' } },
      { url: 'https://example.com/hook', method: 'POST', headers: { Authorization: '***' }, timeout_secs: 30 },
    ).headers).toEqual({ Authorization: 'Bearer new' })
  })

  test('without supplied headers the stored (hidden) ones are sent back unchanged', () => {
    expect(buildWebhookConfigUpdate(
      { method: 'PUT' },
      { url: 'https://example.com/hook', method: 'POST', headers: { Authorization: '***' }, timeout_secs: 30 },
    ).headers).toEqual({ Authorization: '***' })
  })
})

describe('collectHeaderFlag / parseHeaderFlags', () => {
  test('commander collector accumulates repeated flags', () => {
    expect(collectHeaderFlag('A: 1', collectHeaderFlag('B: 2'))).toEqual(['B: 2', 'A: 1'])
  })

  test('no flag leaves headers untouched', () => {
    expect(parseHeaderFlags(undefined)).toEqual({ headers: undefined })
  })

  test('trims spaces and tabs around the name and value', () => {
    expect(parseHeaderFlags(['  X-Foo \t:  \tbar baz \t '])).toEqual({ headers: { 'X-Foo': 'bar baz' } })
  })

  test('parses "Name: value", keeping colons inside the value', () => {
    expect(parseHeaderFlags(['Authorization: Bearer abc', 'X-Url:https://x.example/a:b'])).toEqual({
      headers: { Authorization: 'Bearer abc', 'X-Url': 'https://x.example/a:b' },
    })
  })

  test('rejects a flag without a colon, a bad name, an empty value or a duplicate name', () => {
    expect(parseHeaderFlags(['Authorization'])).toHaveProperty('error')
    expect(parseHeaderFlags(['Bad Name: x'])).toHaveProperty('error')
    expect(parseHeaderFlags(['X-Foo:   '])).toHaveProperty('error')
    expect(parseHeaderFlags(['X-Foo: 1', 'x-foo: 2'])).toHaveProperty('error')
  })

  test('rejects control characters in a value, but allows tabs, spaces and non-ASCII text', () => {
    expect(parseHeaderFlags(['X-Foo: a\nb'])).toHaveProperty('error')
    expect(parseHeaderFlags(['X-Foo: a\u0000b'])).toHaveProperty('error')
    expect(parseHeaderFlags(['X-Foo: a\u007fb'])).toHaveProperty('error')
    // A CR/LF at the edge is rejected, not silently trimmed away.
    expect(parseHeaderFlags(['X-Foo: value\n'])).toHaveProperty('error')
    expect(parseHeaderFlags(['X-Foo: \rvalue'])).toHaveProperty('error')
    expect(parseHeaderFlags(['\nX-Foo: value'])).toHaveProperty('error')
    expect(parseHeaderFlags(['X-Foo: a\tb c'])).toEqual({ headers: { 'X-Foo': 'a\tb c' } })
    expect(parseHeaderFlags(['X-Foo: caf\u00e9'])).toEqual({ headers: { 'X-Foo': 'caf\u00e9' } })
  })

  test('errors never repeat the raw flag, which may contain a credential', () => {
    for (const bad of [
      'Bad Name: secret-token',
      'Authorization Bearer secret-token',
      'secret-token:',
      'secret-token: ***',
      'secret-token: a\nb',
    ]) {
      const result = parseHeaderFlags([bad])
      expect(result).toHaveProperty('error')
      expect(JSON.stringify(result)).not.toContain('secret-token')
    }
    const duplicate = parseHeaderFlags(['secret-token: 1', 'Secret-Token: 2'])
    expect(duplicate).toHaveProperty('error')
    expect(JSON.stringify(duplicate)).not.toContain('ecret-')
  })

  test('keeps a header named __proto__ as an own property', () => {
    const result = parseHeaderFlags(['__proto__: x'])
    expect('headers' in result && Object.keys(result.headers ?? {})).toEqual(['__proto__'])
    expect('headers' in result && JSON.stringify(result.headers)).toBe('{"__proto__":"x"}')
  })

  test('rejects the hidden placeholder as a real value', () => {
    expect(parseHeaderFlags(['Authorization: ***'])).toHaveProperty('error')
  })
})

describe('withHeaderHint', () => {
  const refusal = "'url' was changed, so the masked value at 'headers.Authorization' must be entered again instead of '***'"

  test('points at --header when the server refuses to restore hidden headers', () => {
    expect(withHeaderHint(refusal, {})).toBe(`${refusal}. Pass each header again with --header "Name: value".`)
  })

  test('leaves other errors, and updates that already pass --header, alone', () => {
    expect(withHeaderHint('Provider not found', {})).toBe('Provider not found')
    expect(withHeaderHint(refusal, { header: ['A: b'] })).toBe(refusal)
  })
})

describe('parseProviderIds', () => {
  test('parses a comma-separated list of IDs', () => {
    expect(parseProviderIds('1,2,3')).toEqual([1, 2, 3])
  })

  test('trims whitespace around each ID', () => {
    expect(parseProviderIds(' 1 , 2 ,3')).toEqual([1, 2, 3])
  })

  test('ignores empty segments from trailing/duplicate commas', () => {
    expect(parseProviderIds('1,,2,')).toEqual([1, 2])
  })

  test('a single ID parses to a one-element array', () => {
    expect(parseProviderIds('42')).toEqual([42])
  })
})
