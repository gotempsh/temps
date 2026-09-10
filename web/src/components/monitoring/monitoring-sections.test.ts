// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MONITORING_SECTIONS,
  monitoringSectionLabel,
} from './monitoring-sections'

describe('monitoring sections', () => {
  test('keeps project alert rules separate from global alert settings', () => {
    expect(MONITORING_SECTIONS.map((section) => section.id)).toEqual([
      'alerts',
      'rules',
      'alarms',
      'notifications',
    ])
  })

  test('provides the user-facing alert rules label', () => {
    expect(monitoringSectionLabel('rules')).toBe('Alert rules')
  })
})
