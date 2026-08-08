import { test, expect, describe } from 'bun:test'
import {
  parseEnabledFlag,
  hasSlackConfigChanges,
  hasEmailConfigChanges,
  hasWebhookConfigChanges,
  buildSlackConfigUpdate,
  buildEmailConfigUpdate,
  buildWebhookConfigUpdate,
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
})
