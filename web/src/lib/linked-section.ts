// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  SETTINGS_SECTION_PARAM,
  settingsSectionDomId,
} from '@/lib/project-settings-sections'

/**
 * DOM ids a URL asks the page to scroll to, most specific first.
 *
 * A link to a setting must land on that setting, not on the top of its page:
 * `/settings/security#vulnerability-scanning` or, on the combined project
 * settings pages, `settings/delivery?section=build`. Both forms resolve here.
 */
export function linkedSectionTargets(search: string, hash: string): string[] {
  const targets: string[] = []
  const fragment = safeDecode(hash.replace(/^#/, ''))
  if (fragment) targets.push(fragment)
  const section = new URLSearchParams(search).get(SETTINGS_SECTION_PARAM)
  if (section) targets.push(settingsSectionDomId(section), section)
  return targets
}

function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}
