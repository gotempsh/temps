// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  DEFAULT_MONITORING_SECTION,
  MONITORING_SECTIONS,
  monitoringSectionLabel,
} from './monitoring-sections'

describe('monitoring sections', () => {
  test('opens on what is firing, then the rules, then what raises alerts', () => {
    expect(MONITORING_SECTIONS.map((section) => section.id)).toEqual([
      'alarms',
      'rules',
      'alerts',
      'notifications',
    ])
    expect(DEFAULT_MONITORING_SECTION).toBe('alarms')
  })

  test('names each section as its tab does', () => {
    expect(MONITORING_SECTIONS.map((section) => section.label)).toEqual([
      'Alarms',
      'Rules',
      'Events',
      'Preferences',
    ])
  })

  test('retains the label for the dedicated Server route', () => {
    expect(monitoringSectionLabel('server')).toBe('Server')
  })

  test('does not call delivery preferences "Notifications" or "Channels"', () => {
    // Channels is the tab for where notifications go; this one is how.
    expect(monitoringSectionLabel('notifications')).toBe('Preferences')
  })

  test('has no label for a section that does not exist', () => {
    expect(monitoringSectionLabel('nope')).toBeUndefined()
  })
})
