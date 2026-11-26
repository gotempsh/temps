// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildAdminGate, deepMerge, mergeSettings, settingsAssignments, touchedSections } from './advanced.js'
import { registerSettingsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const current = {
  external_url: 'https://temps.example',
  shield: { enabled: false, mode: 'monitor' },
  self_update: null,
  trusted_proxy_cidrs: [],
}

describe('settings apply helpers', () => {
  test('assignments parse JSON and text, and a bare field takes the stdin value', () => {
    expect(settingsAssignments(['shield.enabled=true', 'external_url=https://x.example'], undefined)).toEqual([
      { path: ['shield', 'enabled'], value: true },
      { path: ['external_url'], value: 'https://x.example' },
    ])
    expect(settingsAssignments(['geo.maxmind_license_key'], 'secret')).toEqual([
      { path: ['geo', 'maxmind_license_key'], value: 'secret' },
    ])
    expect(() => settingsAssignments(['geo.key'], undefined)).toThrow(/--value-stdin/)
    expect(() => settingsAssignments(['a=1'], 'x')).toThrow(/needs a field without/)
    expect(() => settingsAssignments(['a', 'b'], 'x')).toThrow(/Only one field/)
  })

  test('touchedSections refuses a setting AppSettings does not have', () => {
    expect(touchedSections(settingsAssignments(['shield.enabled=true'], undefined), { external_url: 'x' })).toEqual([
      'external_url',
      'shield',
    ])
    expect(() => touchedSections(settingsAssignments(['shelter.enabled=true'], undefined))).toThrow(/Unknown setting "shelter"/)
  })

  test('mergeSettings keeps every other setting, refuses unknown nested fields, and fills an unset section', () => {
    const merged = mergeSettings(current, undefined, settingsAssignments(['shield.mode=block'], undefined))
    expect(merged).toEqual({ ...current, shield: { enabled: false, mode: 'block' } })
    expect(() => mergeSettings(current, undefined, settingsAssignments(['shield.mood=block'], undefined))).toThrow(/Unknown field "mood"/)
    expect(mergeSettings(current, undefined, settingsAssignments(['self_update.enabled=true'], undefined)).self_update).toEqual({
      enabled: true,
    })
    expect(mergeSettings(current, { external_url: 'https://y.example' }, []).external_url).toBe('https://y.example')
  })

  test('a settings file merges into sections field by field; scalars, lists and null replace', () => {
    const withGeo = { ...current, geo: { refresh_interval_hours: 24, stale_lookup_days: 30 } }
    expect(mergeSettings(withGeo, { geo: { refresh_interval_hours: 8 } }, []).geo).toEqual({ refresh_interval_hours: 8, stale_lookup_days: 30 })
    expect(deepMerge({ a: { b: 1, c: [1] }, d: 1 }, { a: { c: [2] }, d: null })).toEqual({ a: { b: 1, c: [2] }, d: null })
    expect(deepMerge({ a: { b: 1 } }, { a: 'flat' })).toEqual({ a: 'flat' })
  })

  test('the write-only MaxMind key can be set in an existing geo section; other unknown fields still fail', () => {
    const withGeo = { ...current, geo: { refresh_interval_hours: 24 } }
    expect(mergeSettings(withGeo, undefined, settingsAssignments(['geo.maxmind_license_key'], 'secret')).geo).toEqual({
      refresh_interval_hours: 24,
      maxmind_license_key: 'secret',
    })
    expect(() => mergeSettings(withGeo, undefined, settingsAssignments(['geo.maxmind_licence_key=x'], undefined))).toThrow(/Unknown field/)
  })

  test('a field under an unset or absent section is checked against the schema', () => {
    // self_update is null in `current`; geo is absent.
    expect(() => mergeSettings(current, undefined, settingsAssignments(['self_update.enabld=true'], undefined))).toThrow(
      /Unknown field "enabld" in self_update. Known fields: channel, enabled/,
    )
    expect(() => mergeSettings(current, undefined, settingsAssignments(['geo.refresh_hours=8'], undefined))).toThrow(
      /Unknown field "refresh_hours" in geo/,
    )
    // The write-only key is accepted there too.
    expect(mergeSettings(current, undefined, settingsAssignments(['geo.maxmind_license_key'], 'k')).geo).toEqual({ maxmind_license_key: 'k' })
  })

  test('a settings file is checked field by field, including a section it sets from null', () => {
    expect(() => mergeSettings(current, { self_update: { enabld: true } }, [])).toThrow(
      /The settings file's self_update section has unknown field\(s\) enabld/,
    )
    expect(mergeSettings(current, { self_update: { enabled: true } }, []).self_update).toEqual({ enabled: true })
    // A section the file sets to null, then an assignment under it, still checks the name.
    expect(() => mergeSettings(current, { shield: null }, settingsAssignments(['shield.mood=x'], undefined))).toThrow(
      /Unknown field "mood" in shield/,
    )
  })

  test('the committed section field list matches openapi.json', async () => {
    const { renderSectionFields, settingsSectionFields, SPEC_FILE, OUTPUT_FILE } = await import('../../../scripts/generate-settings-fields.js')
    const expected = renderSectionFields(settingsSectionFields(await Bun.file(SPEC_FILE).json()))
    expect(await Bun.file(OUTPUT_FILE).text()).toBe(expected)
  })

  test('a known section the GET response leaves out is created from the assignment', () => {
    expect(mergeSettings(current, undefined, settingsAssignments(['geo.refresh_interval_hours=8'], undefined)).geo).toEqual({
      refresh_interval_hours: 8,
    })
  })
})

describe('buildAdminGate', () => {
  const gate = { allowed_hosts: ['temps.example'], allowed_ips: ['10.0.0.0/8'], trust_forwarded_for: false }

  test('no change flag means show, not save', () => {
    expect(buildAdminGate(gate, {})).toBeNull()
  })

  test('a list flag replaces that list and keeps the others', () => {
    expect(buildAdminGate(gate, { allowedIp: ['192.0.2.1'] })).toEqual({ ...gate, allowed_ips: ['192.0.2.1'] })
    expect(buildAdminGate(gate, { clearHosts: true, trustForwardedFor: true })).toEqual({
      allowed_hosts: [],
      allowed_ips: ['10.0.0.0/8'],
      trust_forwarded_for: true,
    })
    expect(() => buildAdminGate(gate, { allowedIp: ['a'], clearIps: true })).toThrow(/not both/)
  })
})

describe('temps settings (advanced)', () => {
  test('fields lists every setting; fields <path> prints a value', async () => {
    const list = await runWithApi(registerSettingsCommands, ['settings', 'fields', '--json'], {})
    expect(Object.keys(JSON.parse(list.stdout))).toContain('otel_ingest')
    const one = await runWithApi(registerSettingsCommands, ['settings', 'fields', 'shield.mode'], { 'GET /settings': current })
    expect(one.stdout).toContain('monitor')
  })

  test('apply sends the full current settings with the change', async () => {
    const run = await runWithApi(registerSettingsCommands, ['settings', 'apply', 'shield.enabled=true'], {
      'GET /settings': current,
      'PUT /settings': { success: true, message: 'ok' },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([{ request: 'PUT /settings', body: { ...current, shield: { enabled: true, mode: 'monitor' } } }])
  })

  test('apply asks before a setting that can lock you out, and refuses without a terminal', async () => {
    const run = await runWithApi(
      registerSettingsCommands,
      ['settings', 'apply', 'external_url=https://other.example'],
      { 'GET /settings': current, 'PUT /settings': { success: true, message: 'ok' } },
      { stdin: '' },
    )
    expect(run.error?.message).toMatch(/not an interactive terminal/)
    expect(run.requests).toEqual(['GET /settings'])
  })

  test('a sensitive apply rebuilds the body on a fresh read, keeping a change saved meanwhile', async () => {
    let reads = 0
    const run = await runWithApi(registerSettingsCommands, ['settings', 'apply', 'external_url=https://other.example', '--yes'], {
      'GET /settings': () => (++reads === 1 ? current : { ...current, trusted_proxy_cidrs: ['10.0.0.0/8'] }),
      'PUT /settings': { success: true, message: 'ok' },
    })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /settings', 'GET /settings', 'PUT /settings'])
    expect(run.bodies[0]?.body).toEqual({ ...current, external_url: 'https://other.example', trusted_proxy_cidrs: ['10.0.0.0/8'] })
  })

  test('apply reads a secret value from stdin', async () => {
    const run = await runWithApi(
      registerSettingsCommands,
      ['settings', 'apply', 'shield.mode', '--value-stdin'],
      { 'GET /settings': current, 'PUT /settings': { success: true, message: 'ok' } },
      { stdin: 'block' },
    )
    expect(run.error).toBeUndefined()
    expect((run.bodies[0]?.body as { shield: { mode: string } }).shield.mode).toBe('block')
  })

  test('admin-gate shows, and saves a new list after --yes', async () => {
    const gate = { allowed_hosts: [], allowed_ips: [], trust_forwarded_for: false, editable: true, source: 'db' }
    const routes = { 'GET /admin/gate-settings': gate, 'PATCH /admin/gate-settings': gate }
    const show = await runWithApi(registerSettingsCommands, ['settings', 'admin-gate', '--json'], routes)
    expect(JSON.parse(show.stdout).source).toBe('db')
    const save = await runWithApi(
      registerSettingsCommands,
      ['settings', 'admin-gate', '--allowed-ip', '10.0.0.0/8', '--allowed-ip', '192.0.2.1', '--yes'],
      routes,
    )
    expect(save.bodies[0]?.body).toEqual({ allowed_hosts: [], allowed_ips: ['10.0.0.0/8', '192.0.2.1'], trust_forwarded_for: false })
  })

  test('admin-gate refuses to edit a gate set by the environment', async () => {
    const run = await runWithApi(registerSettingsCommands, ['settings', 'admin-gate', '--clear-ips', '--yes'], {
      'GET /admin/gate-settings': { allowed_hosts: [], allowed_ips: ['10.0.0.1'], trust_forwarded_for: false, editable: false, source: 'env' },
    })
    expect(run.error?.message).toMatch(/Environment variables/)
    expect(run.requests).toEqual(['GET /admin/gate-settings'])
  })

  test('telemetry reads, enables and disables', async () => {
    const status = { enabled: true, can_manage: true, default_enabled: true, admin_preference: null, anonymous_id: null }
    const routes = { 'GET /settings/telemetry': status, 'PATCH /settings/telemetry': { ...status, enabled: false } }
    const read = await runWithApi(registerSettingsCommands, ['settings', 'telemetry', '--json'], routes)
    expect(read.requests).toEqual(['GET /settings/telemetry'])
    const off = await runWithApi(registerSettingsCommands, ['settings', 'telemetry', '--disable'], routes)
    expect(off.bodies[0]?.body).toEqual({ enabled: false })
  })

  test('ci-builder set merges assignments onto the saved settings and takes the token from stdin', async () => {
    const saved = { owner: 'acme', repository: 'builder', limits: { concurrency: 1 } }
    const run = await runWithApi(
      registerSettingsCommands,
      ['settings', 'ci-builder', 'set', 'limits.concurrency=3', '--dispatch-token-stdin'],
      {
        'GET /settings/ci-builder': { configured: true, setup_path: '/settings/ci', settings: saved },
        'PUT /settings/ci-builder': { configured: true, setup_path: '/settings/ci' },
      },
      { stdin: 'tok' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ ...saved, limits: { concurrency: 3 }, dispatch_token: 'tok' })
  })

  test('ci-builder set refuses an unknown field', async () => {
    const run = await runWithApi(registerSettingsCommands, ['settings', 'ci-builder', 'set', 'ownr=acme'], {})
    expect(run.error?.message).toMatch(/Unknown CI builder field "ownr"/)
  })

  test('ci-builder provision asks first; disk-status, public-ingress, refresh-routes and weekly-digest call their endpoint', async () => {
    const routes = {
      'POST /settings/ci-builder/provision': new Response(null, { status: 204 }),
      'GET /settings/disk-status': { used_percent: 40 },
      'GET /settings/public-ingress': { url: 'https://temps.example' },
      'POST /settings/routes/refresh': { routes: 3 },
      'POST /weekly-digest/trigger': { success: true },
    }
    const refused = await runWithApi(registerSettingsCommands, ['settings', 'ci-builder', 'provision'], routes, { stdin: '' })
    expect(refused.requests).toEqual([])
    const provision = await runWithApi(registerSettingsCommands, ['settings', 'ci-builder', 'provision', '--yes'], routes)
    expect(provision.requests).toEqual(['POST /settings/ci-builder/provision'])
    for (const [argv, request] of [
      ['disk-status', 'GET /settings/disk-status'],
      ['public-ingress', 'GET /settings/public-ingress'],
      ['refresh-routes', 'POST /settings/routes/refresh'],
      ['weekly-digest', 'POST /weekly-digest/trigger'],
    ] as const) {
      const run = await runWithApi(registerSettingsCommands, ['settings', argv, '--json'], routes)
      expect(run.error).toBeUndefined()
      expect(run.requests).toEqual([request])
    }
  })
})
