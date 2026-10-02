// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { gatewayStatusSummary } from './preview-gateway-status'

describe('gatewayStatusSummary', () => {
  test('a disabled gateway reads as disabled, not as missing', () => {
    expect(
      gatewayStatusSummary(false, { running: false, present: false })
    ).toEqual({ label: 'Disabled', tone: 'muted' })
  })

  test('a disabled gateway whose container survived says so', () => {
    expect(
      gatewayStatusSummary(false, { running: true, present: true })
    ).toEqual({
      label: 'Disabled, but its container is still present',
      tone: 'warning',
    })
  })

  test('an enabled gateway reports its container state', () => {
    expect(
      gatewayStatusSummary(true, { running: true, present: true })
    ).toEqual({ label: 'Running', tone: 'ok' })
    expect(
      gatewayStatusSummary(true, { running: false, present: true })
    ).toEqual({ label: 'Stopped', tone: 'warning' })
    expect(
      gatewayStatusSummary(undefined, { running: false, present: false })
    ).toEqual({ label: 'Not deployed', tone: 'error' })
  })
})
