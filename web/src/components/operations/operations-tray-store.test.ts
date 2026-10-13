// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  getOperationsTrayState,
  openOperationsTray,
  setOperationsTrayOpen,
  trackLocalOperation,
} from './operations-tray-store'

describe('operations tray store', () => {
  test('openOperationsTray opens the tray from anywhere', () => {
    setOperationsTrayOpen(false)
    openOperationsTray()
    expect(getOperationsTrayState().open).toBe(true)
    setOperationsTrayOpen(false)
    expect(getOperationsTrayState().open).toBe(false)
  })

  test('local entries are added newest first and removed on settle', () => {
    const first = trackLocalOperation({ title: 'Restart A', context: null })
    const second = trackLocalOperation({ title: 'Restart B', context: null })
    expect(getOperationsTrayState().local.map((op) => op.title)).toEqual([
      'Restart B',
      'Restart A',
    ])
    first()
    expect(getOperationsTrayState().local.map((op) => op.title)).toEqual([
      'Restart B',
    ])
    second()
    expect(getOperationsTrayState().local).toHaveLength(0)
  })
})
