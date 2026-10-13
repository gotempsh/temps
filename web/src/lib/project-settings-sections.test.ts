// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  LEGACY_PROJECT_ROUTES,
  SETTINGS_SECTION_IDS,
  legacyProjectRouteTarget,
  renameProjectRoutePrefix,
  requestedSettingsSection,
  settingsSectionHref,
  type CombinedSettingsPage,
} from './project-settings-sections'

describe('settings section links', () => {
  test('builds absolute links that open one section', () => {
    expect(settingsSectionHref('app', 'automation', 'cron-jobs')).toBe(
      '/projects/app/settings/automation?section=cron-jobs'
    )
  })

  test('accepts only sections the page actually has', () => {
    expect(requestedSettingsSection('delivery', '?section=feature-flags')).toBe(
      'feature-flags'
    )
    expect(
      requestedSettingsSection('delivery', '?section=cron-jobs')
    ).toBeUndefined()
    expect(requestedSettingsSection('delivery', '')).toBeUndefined()
    expect(
      requestedSettingsSection(
        'variables',
        new URLSearchParams('section=secrets')
      )
    ).toBe('secrets')
  })

  test('section ids are unique across pages, so a DOM id is never reused', () => {
    const all = Object.values(SETTINGS_SECTION_IDS).flat()
    expect(new Set(all).size).toBe(all.length)
  })
})

describe('legacyProjectRouteTarget', () => {
  test('every legacy settings route lands on a section that exists', () => {
    for (const target of Object.values(LEGACY_PROJECT_ROUTES)) {
      const [path, query] = target.split('?')
      if (!path.startsWith('settings/')) continue
      const page = path.slice('settings/'.length) as CombinedSettingsPage
      expect(SETTINGS_SECTION_IDS[page]).toBeDefined()
      expect(requestedSettingsSection(page, query ?? '')).toBeDefined()
    }
  })

  test('standalone pages map to their canonical page and section', () => {
    expect(legacyProjectRouteTarget('flags', '')).toBe(
      'settings/delivery?section=feature-flags'
    )
    expect(legacyProjectRouteTarget('settings/cron-jobs', '')).toBe(
      'settings/automation?section=cron-jobs'
    )
    expect(legacyProjectRouteTarget('setup', '')).toBe(
      'settings/general?section=setup'
    )
    expect(legacyProjectRouteTarget('settings/environment-variables', '')).toBe(
      'environment-variables'
    )
    expect(legacyProjectRouteTarget('settings/domains', '')).toBe('domains')
  })

  test('the old build page tab becomes the matching section', () => {
    expect(legacyProjectRouteTarget('settings/build', '?tab=previews')).toBe(
      'settings/delivery?section=previews'
    )
    expect(legacyProjectRouteTarget('build', '?tab=deploy')).toBe(
      'settings/delivery?section=deployment'
    )
    expect(legacyProjectRouteTarget('build', '?tab=nonsense')).toBe(
      'settings/delivery?section=build'
    )
  })

  test('other query parameters survive the redirect', () => {
    expect(
      legacyProjectRouteTarget('settings/environment-variables', '?q=DATABASE')
    ).toBe('environment-variables?q=DATABASE')
    expect(legacyProjectRouteTarget('agents', '?tab=runs')).toBe(
      'settings/automation?section=agents&tab=runs'
    )
  })
})

describe('renameProjectRoutePrefix', () => {
  test('keeps detail ids and encoding under the renamed segment', () => {
    expect(
      renameProjectRoutePrefix('/projects/app/logs', 'logs', 'request-logs')
    ).toBe('/projects/app/request-logs')
    expect(
      renameProjectRoutePrefix(
        '/projects/app/logs/req%2F42',
        'logs',
        'request-logs'
      )
    ).toBe('/projects/app/request-logs/req%2F42')
  })

  test('ignores paths that are not under the renamed segment', () => {
    expect(
      renameProjectRoutePrefix(
        '/projects/app/telemetry-logs',
        'logs',
        'request-logs'
      )
    ).toBeUndefined()
    expect(
      renameProjectRoutePrefix('/settings/logs', 'logs', 'request-logs')
    ).toBeUndefined()
  })
})
