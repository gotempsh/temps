// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { resolveProjectLocation } from '@/lib/project-navigation'
import { installBrowserWindow } from '@/test/dom'
import { ProjectSectionTabs } from './ProjectSectionTabs'

installBrowserWindow()
afterEach(cleanup)

function renderTabs(route: string) {
  return render(
    <MemoryRouter initialEntries={[`/projects/synthetic-api/${route}`]}>
      <ProjectSectionTabs
        projectSlug="synthetic-api"
        location={resolveProjectLocation(route)}
      />
    </MemoryRouter>
  )
}

test('edge rules is a tab of Security & access, current on its page', () => {
  const view = renderTabs('settings/edge-rules')
  const nav = view.getByRole('navigation', { name: 'Security & access pages' })
  const tab = view.getByRole('link', { name: 'Edge rules' })
  expect(nav.contains(tab)).toBe(true)
  expect(tab.getAttribute('aria-current')).toBe('page')
  expect(tab.getAttribute('href')).toBe(
    '/projects/synthetic-api/settings/edge-rules'
  )
})

test('a rule opened for editing keeps the Edge rules tab current', () => {
  const view = renderTabs('settings/edge-rules/new')
  expect(
    view.getByRole('link', { name: 'Edge rules' }).getAttribute('aria-current')
  ).toBe('page')
})

test('tab icons sit before the label and drop on a phone', () => {
  const view = renderTabs('settings/security')
  const icon = view.getByRole('link', { name: 'Protection' }).firstElementChild
  expect(icon?.tagName.toLowerCase()).toBe('svg')
  expect(icon?.getAttribute('aria-hidden')).toBe('true')
  expect(icon?.getAttribute('class')).toContain('max-sm:hidden')
})

test('on a narrow strip, the current tab is scrolled into view', () => {
  // A 300px strip whose tabs are 100px apart: Check-ins, the sixth, starts
  // past the edge.
  const original = HTMLElement.prototype.getBoundingClientRect
  HTMLElement.prototype.getBoundingClientRect = function (this: HTMLElement) {
    if (this.tagName === 'NAV')
      return { left: 0, right: 300, width: 300 } as DOMRect
    const index = Array.from(this.parentElement?.children ?? []).indexOf(this)
    const left = index * 100 - (this.parentElement?.scrollLeft ?? 0)
    return { left, right: left + 80, width: 80 } as DOMRect
  }
  try {
    const view = renderTabs('check-ins')
    const nav = view.getByRole('navigation', { name: 'Monitoring pages' })
    expect(
      view.getByRole('link', { name: 'Check-ins' }).getAttribute('aria-current')
    ).toBe('page')
    // Centred: 500 - 0 - (300 - 80) / 2.
    expect(nav.scrollLeft).toBe(390)
  } finally {
    HTMLElement.prototype.getBoundingClientRect = original
  }
})

test('a tab already in view leaves the strip where it is', () => {
  const view = renderTabs('metrics/explore')
  expect(
    view.getByRole('navigation', { name: 'Monitoring pages' }).scrollLeft
  ).toBe(0)
})
