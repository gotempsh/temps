// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { getProjectTourCardStyle } from '@/lib/project-tour'

describe('getProjectTourCardStyle', () => {
  test('uses viewport gutters and a bottom sheet on mobile', () => {
    expect(
      getProjectTourCardStyle({
        isMobile: true,
        anchor: null,
        viewportWidth: 320,
        viewportHeight: 568,
      })
    ).toEqual({ left: 16, right: 16, bottom: 16 })
  })

  test('never places the desktop coachmark outside a narrow viewport', () => {
    const style = getProjectTourCardStyle({
      isMobile: false,
      anchor: { top: 800, right: 1_200 },
      viewportWidth: 1_024,
      viewportHeight: 768,
    })

    expect(style.left).toBe(692)
    expect(style.top).toBe(576)
  })
})
