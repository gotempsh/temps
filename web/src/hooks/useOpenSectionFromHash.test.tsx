// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { MemoryRouter, useSearchParams } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import {
  openSettingsSection,
  useScrollToLinkedSection,
} from './useOpenSectionFromHash'

installBrowserWindow()
afterEach(cleanup)

let scrolledTo: string[] = []
const originalScroll = Element.prototype.scrollIntoView
beforeEach(() => {
  scrolledTo = []
  Element.prototype.scrollIntoView = function (this: Element) {
    scrolledTo.push(this.id)
  }
})
afterEach(() => {
  Element.prototype.scrollIntoView = originalScroll
})

/** A settings page whose card renders after its group, like a slow query. */
function Page() {
  useScrollToLinkedSection()
  const [loaded, setLoaded] = useState(false)
  return (
    <section id="settings-section-build">
      <button type="button" onClick={() => setLoaded(true)}>
        Load
      </button>
      {loaded && <div id="setting-docker-compose">Docker Compose</div>}
    </section>
  )
}

function renderAt(url: string) {
  const view = render(
    <MemoryRouter initialEntries={[url]}>
      <Page />
    </MemoryRouter>
  )
  return { showSetting: () => fireEvent.click(view.getByText('Load')) }
}

test('lands on the group first, then on the setting once it renders', async () => {
  const { showSetting } = renderAt(
    '/settings/delivery?section=build#setting-docker-compose'
  )
  await waitFor(() => expect(scrolledTo).toEqual(['settings-section-build']))
  showSetting()
  await waitFor(() =>
    expect(scrolledTo).toEqual([
      'settings-section-build',
      'setting-docker-compose',
    ])
  )
})

test('stays on the group when this service does not have the setting', async () => {
  const { showSetting } = renderAt(
    '/settings/delivery?section=build#setting-compose-services'
  )
  await waitFor(() => expect(scrolledTo).toEqual(['settings-section-build']))
  showSetting()
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(scrolledTo).toEqual(['settings-section-build'])
})

/** A page that changes a query parameter of its own, like a tab. */
function TabbedPage() {
  useScrollToLinkedSection()
  const [, setSearchParams] = useSearchParams()
  return (
    <section id="settings-section-agents">
      <button
        type="button"
        onClick={() =>
          setSearchParams((current) => {
            const next = new URLSearchParams(current)
            next.set('tab', 'runs')
            return next
          })
        }
      >
        Runs
      </button>
    </section>
  )
}

test('changing another query parameter does not scroll back to the section', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/settings/automation?section=agents']}>
      <TabbedPage />
    </MemoryRouter>
  )
  await waitFor(() => expect(scrolledTo).toEqual(['settings-section-agents']))
  fireEvent.click(view.getByText('Runs'))
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(scrolledTo).toEqual(['settings-section-agents'])
})

test('the highlight leaves classes the element already had', () => {
  const card = document.createElement('div')
  card.id = 'linked-card'
  card.className = 'rounded-lg border'
  document.body.append(card)
  const timers: (() => void)[] = []
  const originalSetTimeout = window.setTimeout
  window.setTimeout = ((callback: () => void) => {
    timers.push(callback)
    return timers.length
  }) as typeof window.setTimeout
  try {
    openSettingsSection('linked-card')
    // A second link before the first highlight ends restarts it.
    openSettingsSection('linked-card')
    expect(card.classList.contains('ring-2')).toBe(true)
    timers[0]()
    timers[1]()
  } finally {
    window.setTimeout = originalSetTimeout
    card.remove()
  }
  expect(card.className).toBe('rounded-lg border')
})
