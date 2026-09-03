// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  getProjectTourCardStyle,
  getProjectTourNavigationTarget,
  isProjectTourHomePage,
} from '@/lib/project-tour'

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

describe('project tour navigation guards', () => {
  test('auto-starts only from the project home routes', () => {
    expect(isProjectTourHomePage('example', '/projects/example')).toBe(true)
    expect(isProjectTourHomePage('example', '/projects/example/project')).toBe(
      true
    )
    expect(
      isProjectTourHomePage('example', '/projects/example/deployments')
    ).toBe(false)
    expect(isProjectTourHomePage(undefined, '/projects/example')).toBe(false)
  })

  test('does not navigate repeatedly after a tour route redirects', () => {
    const target = getProjectTourNavigationTarget({
      active: true,
      slug: 'example',
      route: 'metrics',
      lastTarget: null,
    })

    expect(target).toBe('/projects/example/metrics')
    expect(
      getProjectTourNavigationTarget({
        active: true,
        slug: 'example',
        route: 'metrics',
        lastTarget: target,
      })
    ).toBeNull()
    expect(
      getProjectTourNavigationTarget({
        active: false,
        slug: 'example',
        route: 'metrics',
        lastTarget: null,
      })
    ).toBeNull()
  })
})
