// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  buildAutomationSettingsUpdate,
  describeGeoRefreshState,
  type GeoStatusSnapshot,
} from './index.js'

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
    const current = { letsencrypt: { email: 'ops@example.com', environment: 'staging' } }
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
    const current = { rate_limiting: { enabled: false, max_requests_per_minute: 30 } }
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
  test('a single ceiling flag carries the other ceilings forward', () => {
    const result = buildAutomationSettingsUpdate(
      { maxMemoryLimitMb: '4096' },
      {
        tenant_resource_ceilings: {
          max_memory_limit_mb: 0,
          max_concurrent_connections: 200,
          allow_unlimited_request_timeouts: false,
        },
      },
    )
    expect(result).toEqual({
      updates: {
        tenant_resource_ceilings: {
          max_memory_limit_mb: 4096,
          // Would silently become 0 ("no ceiling") if not carried forward.
          max_concurrent_connections: 200,
          allow_unlimited_request_timeouts: false,
        },
      },
    })
  })

  test('ceilings default to unenforced when the server has none set', () => {
    const result = buildAutomationSettingsUpdate({ maxMemoryLimitMb: '512' }, undefined)
    expect(result).toEqual({
      updates: {
        tenant_resource_ceilings: {
          max_memory_limit_mb: 512,
          max_concurrent_connections: 0,
          allow_unlimited_request_timeouts: true,
        },
      },
    })
  })

  test('rejects a negative ceiling instead of writing it', () => {
    const result = buildAutomationSettingsUpdate({ maxMemoryLimitMb: '-1' }, undefined)
    expect(result).toEqual({
      error: '--max-memory-limit-mb must be a non-negative number (0 = no ceiling), got "-1"',
    })
  })

  test('rejects a non-boolean --allow-unlimited-timeouts', () => {
    const result = buildAutomationSettingsUpdate({ allowUnlimitedTimeouts: 'yes' }, undefined)
    expect(result).toEqual({
      error: '--allow-unlimited-timeouts must be true or false, got "yes"',
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

describe('describeGeoRefreshState', () => {
  const base: GeoStatusSnapshot = {
    license_key_configured: true,
    refresh_interval_hours: 24,
    is_stale: false,
  }

  test('reports a current database as OK with its age and source', () => {
    const state = describeGeoRefreshState({
      ...base,
      last_check_at: '2026-09-17T03:14:00Z',
      last_check_status: 'ok',
      age_days: 2,
      source: 'maxmind_official',
    })
    expect(state.level).toBe('ok')
    expect(state.headline).toBe('Geolocation data is current')
    expect(state.detail).toContain('maxmind_official')
  })

  test('explains an unlicensed instance instead of showing a blank status', () => {
    const state = describeGeoRefreshState({
      ...base,
      license_key_configured: false,
      last_check_at: '2026-09-17T03:14:00Z',
      last_check_status: 'skipped_no_license_key',
    })
    expect(state.level).toBe('warn')
    expect(state.headline).toBe('Automatic refreshes are not running')
    // Must name the fix, not just the gap.
    expect(state.detail).toContain('temps settings update --setting geo')
    expect(state.detail).toContain('every 24 hours')
  })

  test('surfaces the recorded reason a check failed', () => {
    const state = describeGeoRefreshState({
      ...base,
      last_check_at: '2026-09-17T03:14:00Z',
      last_check_status: 'error',
      last_error: 'HTTP 401',
    })
    expect(state.level).toBe('warn')
    expect(state.headline).toBe('Last refresh check failed')
    expect(state.detail).toContain('HTTP 401')
  })

  test('falls back to a message when no reason was recorded for a failure', () => {
    const state = describeGeoRefreshState({
      ...base,
      last_check_at: '2026-09-17T03:14:00Z',
      last_check_status: 'error',
    })
    expect(state.detail).toContain('No reason was recorded')
  })

  test('distinguishes "never checked, licensed" from "never checked, unlicensed"', () => {
    const licensed = describeGeoRefreshState(base)
    expect(licensed.level).toBe('ok')
    expect(licensed.headline).toBe('No refresh has run yet on this instance')

    const unlicensed = describeGeoRefreshState({ ...base, license_key_configured: false })
    expect(unlicensed.level).toBe('warn')
    expect(unlicensed.detail).toContain('will not download anything')
  })

  test('reports a stale database with its age', () => {
    const state = describeGeoRefreshState({
      ...base,
      last_check_at: '2026-09-17T03:14:00Z',
      last_check_status: 'ok',
      is_stale: true,
      age_days: 120,
    })
    expect(state.level).toBe('warn')
    expect(state.headline).toBe('Geolocation data is stale')
    expect(state.detail).toContain('120 days old')
  })
})
