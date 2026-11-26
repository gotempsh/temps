// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { AUDIT_LOGS_URL } from '@/components/dashboard/primary-navigation'
import { settingsNavigationGroups } from './settings-navigation'
import { withoutRestrictedSettings } from './use-settings-groups'

const urls = (groups: typeof settingsNavigationGroups) =>
  groups.flatMap((group) => group.items.map((item) => item.url))

describe('withoutRestrictedSettings', () => {
  test('an administration role sees every settings page', () => {
    expect(
      urls(withoutRestrictedSettings(settingsNavigationGroups, true))
    ).toEqual(urls(settingsNavigationGroups))
  })

  test('another role does not get an entry to the audit log', () => {
    const visible = withoutRestrictedSettings(settingsNavigationGroups, false)
    expect(urls(visible)).not.toContain(AUDIT_LOGS_URL)
    expect(urls(visible)).toHaveLength(
      urls(settingsNavigationGroups).length - 1
    )
    // The registry itself is left as it was.
    expect(urls(settingsNavigationGroups)).toContain(AUDIT_LOGS_URL)
  })

  test('a group left with nothing is not listed', () => {
    const access = settingsNavigationGroups.find(
      (group) => group.label === 'Access'
    )
    const only = [{ label: 'Access', items: [access!.items[4]] }]
    expect(only[0].items[0].url).toBe(AUDIT_LOGS_URL)
    expect(withoutRestrictedSettings(only, false)).toEqual([])
  })
})
