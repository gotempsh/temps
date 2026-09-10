// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { stepIcon } from './execute-display.js'
import type { StepResult } from '../../api/types.gen.js'

function step(overrides: Partial<StepResult>): StepResult {
  return {
    created_resources: [],
    duration_seconds: 1,
    message: '',
    skipped: false,
    step_id: 'id',
    step_title: 'title',
    success: true,
    ...overrides,
  }
}

describe('stepIcon', () => {
  test('shows a neutral marker for a skipped step regardless of success', () => {
    expect(stepIcon(step({ skipped: true, success: true }))).toContain('○')
  })

  test('shows a neutral marker for a skipped step even when success is false', () => {
    // Skipped takes priority over success/failure — a skipped step is neither.
    expect(stepIcon(step({ skipped: true, success: false }))).toContain('○')
  })

  test('shows a pass icon for a non-skipped successful step', () => {
    expect(stepIcon(step({ skipped: false, success: true }))).not.toContain('○')
  })

  test('distinguishes a failed step from a successful one', () => {
    const passIcon = stepIcon(step({ skipped: false, success: true }))
    const failIcon = stepIcon(step({ skipped: false, success: false }))
    expect(failIcon).not.toBe(passIcon)
  })
})
