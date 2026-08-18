import { test, expect, describe } from 'bun:test'
import { buildAutomationSettingsUpdate } from './index.js'
import type { AppSettings } from '../../api/types.gen.js'

describe('buildAutomationSettingsUpdate', () => {
  test('reports no fields to update when nothing was provided', () => {
    const result = buildAutomationSettingsUpdate({}, undefined)
    expect(result).toEqual({ error: 'No settings to update' })
  })

  test('passes external_url and preview_domain through directly', () => {
    const result = buildAutomationSettingsUpdate(
      { externalUrl: 'https://example.com', previewDomain: '{{slug}}.preview.example.com' },
      undefined,
    )
    expect(result).toEqual({
      updates: {
        external_url: 'https://example.com',
        preview_domain: '{{slug}}.preview.example.com',
      },
    })
  })

  test('falls back to the current letsencrypt email when only mode is given', () => {
    const current: Partial<AppSettings> = { letsencrypt: { email: 'ops@example.com', environment: 'staging' } }
    const result = buildAutomationSettingsUpdate({ letsencryptMode: 'production' }, current)
    expect(result).toEqual({
      updates: { letsencrypt: { email: 'ops@example.com', environment: 'production' } },
    })
  })

  test('defaults letsencrypt environment to staging with no prior settings', () => {
    const result = buildAutomationSettingsUpdate({ letsencryptEmail: 'ops@example.com' }, undefined)
    expect(result).toEqual({
      updates: { letsencrypt: { email: 'ops@example.com', environment: 'staging' } },
    })
  })

  test('parses --rate-limiting-rpm and keeps enabled flag independent', () => {
    const result = buildAutomationSettingsUpdate(
      { rateLimitingEnabled: 'true', rateLimitingRpm: '120' },
      undefined,
    )
    expect(result).toEqual({
      updates: { rate_limiting: { enabled: true, max_requests_per_minute: 120 } },
    })
  })

  test('rate limiting keeps the existing rpm when only enabled is toggled', () => {
    const current: Partial<AppSettings> = { rate_limiting: { enabled: false, max_requests_per_minute: 30 } }
    const result = buildAutomationSettingsUpdate({ rateLimitingEnabled: 'false' }, current)
    expect(result).toEqual({
      updates: { rate_limiting: { enabled: false, max_requests_per_minute: 30 } },
    })
  })

  test('rate limiting defaults to 60 rpm with no prior settings and no --rate-limiting-rpm', () => {
    const result = buildAutomationSettingsUpdate({ rateLimitingEnabled: 'true' }, undefined)
    expect(result).toEqual({
      updates: { rate_limiting: { enabled: true, max_requests_per_minute: 60 } },
    })
  })

  test('"false" string is treated as boolean false, not a truthy string', () => {
    // options.screenshotsEnabled arrives as a raw CLI string; a naive truthy
    // check would treat "false" as enabled.
    const result = buildAutomationSettingsUpdate({ screenshotsEnabled: 'false' }, undefined)
    expect(result).toEqual({ updates: { screenshots: { enabled: false } } })
  })

  test('generic --setting/--value pair updates a known field', () => {
    const result = buildAutomationSettingsUpdate({ setting: 'preview_domain', value: 'x.example.com' }, undefined)
    expect(result).toEqual({ updates: { preview_domain: 'x.example.com' } })
  })

  test('rejects an unknown --setting name without making a partial update', () => {
    const result = buildAutomationSettingsUpdate(
      { externalUrl: 'https://example.com', setting: 'bogus', value: 'x' },
      undefined,
    )
    expect(result).toEqual({ error: 'Unknown setting: bogus' })
  })

  test('combines multiple independent flags into a single patch', () => {
    const result = buildAutomationSettingsUpdate(
      { externalUrl: 'https://example.com', screenshotsEnabled: 'true' },
      undefined,
    )
    expect(result).toEqual({
      updates: {
        external_url: 'https://example.com',
        screenshots: { enabled: true },
      },
    })
  })
  test('--console-force-https maps its three modes onto the tri-state', () => {
    // `auto` must clear the override (null), not omit it — omitting would let
    // `#[serde(default)]` decide, which is the same value but by accident.
    expect(buildAutomationSettingsUpdate({ consoleForceHttps: 'auto' }, undefined)).toEqual({
      updates: { console_force_https: null },
    })
    expect(buildAutomationSettingsUpdate({ consoleForceHttps: 'always' }, undefined)).toEqual({
      updates: { console_force_https: true },
    })
    expect(buildAutomationSettingsUpdate({ consoleForceHttps: 'never' }, undefined)).toEqual({
      updates: { console_force_https: false },
    })
  })

  test('rejects an unknown --console-force-https mode', () => {
    const result = buildAutomationSettingsUpdate({ consoleForceHttps: 'true' }, undefined)
    expect(result).toEqual({
      error: '--console-force-https must be auto, always or never, got "true"',
    })
  })
})
