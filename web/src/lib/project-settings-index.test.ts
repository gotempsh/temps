// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  PROJECT_SETTINGS_INDEX,
  projectHasSetting,
  projectSettingUrl,
  settingPresetOf,
} from './project-settings-index'
import {
  SETTINGS_SECTION_IDS,
  settingsSectionDomId,
  type CombinedSettingsPage,
} from './project-settings-sections'

/** Every non-test component source, concatenated once. */
async function componentSources(): Promise<string> {
  const root = new URL('../', import.meta.url).pathname
  const parts: string[] = []
  for await (const path of new Bun.Glob('**/*.tsx').scan(root)) {
    if (path.includes('.test.')) continue
    parts.push(await Bun.file(`${root}/${path}`).text())
  }
  return parts.join('\n')
}

function escape(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** Combined settings page a `settings/<page>` path belongs to, if any. */
function combinedPage(page: string): CombinedSettingsPage | undefined {
  const name = page.replace(/^settings\//, '')
  return name in SETTINGS_SECTION_IDS
    ? (name as CombinedSettingsPage)
    : undefined
}

describe('PROJECT_SETTINGS_INDEX', () => {
  test('every anchor is rendered by some component', async () => {
    const sources = await componentSources()
    const missing = PROJECT_SETTINGS_INDEX.filter(({ anchor, page }) => {
      // A combined settings page renders its sections' ids itself.
      const combined = combinedPage(page)
      if (combined && anchor.startsWith(settingsSectionDomId(''))) {
        const section = anchor.slice(settingsSectionDomId('').length)
        return !(SETTINGS_SECTION_IDS[combined] as readonly string[]).includes(
          section
        )
      }
      // A literal id (`id="setting-docker-compose"`, `id: 'shield'`).
      if (new RegExp(`["'\`]${escape(anchor)}["'\`]`).test(sources))
        return false
      // `SettingToggleRow id="x"` renders its row as `setting-x`.
      const row = anchor.replace(/^setting-/, '')
      return !new RegExp(`<SettingToggleRow\\s+id="${escape(row)}"`).test(
        sources
      )
    }).map((entry) => entry.anchor)
    expect(missing).toEqual([])
  })

  test('fallback sections exist on their page', () => {
    for (const entry of PROJECT_SETTINGS_INDEX) {
      if (!entry.section) continue
      const combined = combinedPage(entry.page)
      const sections: readonly string[] = combined
        ? SETTINGS_SECTION_IDS[combined]
        : []
      // Pair the anchor with the result so a failure names the entry.
      expect([entry.anchor, sections.includes(entry.section)]).toEqual([
        entry.anchor,
        true,
      ])
    }
  })

  test('anchors and titles are unique', () => {
    const anchors = PROJECT_SETTINGS_INDEX.map((entry) => entry.anchor)
    const titles = PROJECT_SETTINGS_INDEX.map((entry) => entry.title)
    expect(new Set(anchors).size).toBe(anchors.length)
    expect(new Set(titles).size).toBe(titles.length)
  })
})

describe('projectSettingUrl', () => {
  test('names the page, the fallback group and the setting', () => {
    expect(
      projectSettingUrl({
        anchor: 'setting-docker-compose',
        title: 'Docker Compose',
        page: 'settings/delivery',
        section: 'build',
        keywords: [],
      })
    ).toBe('settings/delivery?section=build#setting-docker-compose')
  })

  test('keeps the query a page needs and works without a group', () => {
    expect(
      projectSettingUrl({
        anchor: 'setting-environment-volumes',
        title: 'Volumes',
        page: 'environments',
        query: { view: 'settings' },
        keywords: [],
      })
    ).toBe('environments?view=settings#setting-environment-volumes')
    expect(
      projectSettingUrl({
        anchor: 'setting-identity',
        title: 'Service name and slug',
        page: 'settings/general',
        keywords: [],
      })
    ).toBe('settings/general#setting-identity')
  })
})

describe('projectHasSetting', () => {
  test('a setting without a preset belongs to every service', () => {
    expect(projectHasSetting({ preset: 'nextjs' }, {})).toBe(true)
    expect(projectHasSetting({ preset: null }, {})).toBe(true)
  })

  test('a preset-specific setting belongs only to that preset', () => {
    const compose = { preset: 'docker-compose' } as const
    expect(projectHasSetting({ preset: 'docker-compose' }, compose)).toBe(true)
    expect(projectHasSetting({ preset: 'dockercompose' }, compose)).toBe(true)
    expect(projectHasSetting({ preset: 'dockerfile' }, compose)).toBe(false)
    expect(projectHasSetting({ preset: 'nextjs' }, compose)).toBe(false)
    expect(projectHasSetting({}, compose)).toBe(false)
  })

  test('the Compose and Dockerfile settings are tagged with their preset', () => {
    const byPreset = (preset: string) =>
      PROJECT_SETTINGS_INDEX.filter(
        (entry) => entry.preset === settingPresetOf(preset)
      ).map((entry) => entry.title)
    expect(byPreset('docker-compose')).toEqual([
      'Compose file',
      'Docker Compose',
      'Compose public routes',
      'Compose services',
    ])
    expect(byPreset('dockerfile')).toEqual([
      'Dockerfile path',
      'Build context',
      'Container startup',
    ])
  })
})
