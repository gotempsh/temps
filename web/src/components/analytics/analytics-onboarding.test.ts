// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ANALYTICS_ONBOARDING_COPY,
  liveVisitorsPillLabel,
  resolveAnalyticsInstallState,
} from './analytics-onboarding'

describe('analytics install state', () => {
  test('waits for the has-events check before deciding', () => {
    expect(
      resolveAnalyticsInstallState({ isPending: true, isError: false })
    ).toBe('checking')
  })

  test('onboards when the project has never received an event', () => {
    expect(
      resolveAnalyticsInstallState({
        isPending: false,
        isError: false,
        data: { has_events: false },
      })
    ).toBe('not-installed')
  })

  test('renders the view once events have been received', () => {
    expect(
      resolveAnalyticsInstallState({
        isPending: false,
        isError: false,
        data: { has_events: true },
      })
    ).toBe('installed')
  })

  test('falls through to the view when the check itself fails', () => {
    expect(
      resolveAnalyticsInstallState({ isPending: false, isError: true })
    ).toBe('installed')
  })
})

test('every onboarding state names the view and shows a concrete example', () => {
  for (const copy of Object.values(ANALYTICS_ONBOARDING_COPY)) {
    expect(copy.title.length).toBeGreaterThan(0)
    expect(copy.example).toMatch(/for example/)
  }
})

test('the live visitors pill label always says where it leads', () => {
  expect(liveVisitorsPillLabel(0)).toBe(
    'No active visitors right now. Open Live visitors'
  )
  expect(liveVisitorsPillLabel(1)).toBe('1 active visitor. Open Live visitors')
  expect(liveVisitorsPillLabel(12)).toBe(
    '12 active visitors. Open Live visitors'
  )
})
