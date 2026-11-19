// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { useState, type ReactNode } from 'react'
import { MemoryRouter, useSearchParams } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import {
  openSettingsSection,
  useScrollToLinkedSection,
} from './useOpenSectionFromHash'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const originalScrollIntoView = Element.prototype.scrollIntoView
beforeEach(() => {
  // It scrolls every ancestor, the clipped app shell included; the page
  // ended up stuck half off screen. Nothing here may call it.
  Element.prototype.scrollIntoView = function (this: Element) {
    throw new Error(`scrollIntoView called on #${this.id}`)
  }
})
afterEach(() => {
  Element.prototype.scrollIntoView = originalScrollIntoView
})

/**
 * Which section each scroll of the page brought to its top, in order, read
 * back from the offsets: `top:N` for a scroll that matches none.
 */
function scrolledTo(): string[] {
  const page = document.getElementById('page')
  return layout.scrolls.map(({ target, top }) => {
    if (target !== 'page' || !page) return `${target}:${top}`
    const section = [...page.children].find(
      (element) => Number(element.getAttribute('data-top')) - top === 0
    )
    return section?.id ?? `top:${top}`
  })
}

/**
 * The console shell around a page: a frame that clips (`overflow: hidden`,
 * like the project frame) holding the page's own 600px scroll container.
 */
function Shell({
  children,
  height = 3000,
}: {
  children: ReactNode
  height?: number
}) {
  return (
    <div
      id="frame"
      data-scroller=""
      data-client-height="800"
      data-scroll-height="900"
      style={{ overflow: 'hidden' }}
    >
      <div
        id="page"
        data-scroller=""
        data-top="100"
        data-client-height="600"
        data-scroll-height={height}
        style={{ overflowY: 'auto' }}
      >
        {children}
      </div>
    </div>
  )
}

/** A settings page whose card renders after its group, like a slow query. */
function Page() {
  useScrollToLinkedSection()
  const [loaded, setLoaded] = useState(false)
  return (
    <Shell>
      <section id="settings-section-build" data-top="400">
        <button type="button" onClick={() => setLoaded(true)}>
          Load
        </button>
      </section>
      {loaded && (
        <div id="setting-docker-compose" data-top="700">
          Docker Compose
        </div>
      )}
    </Shell>
  )
}

function renderAt(url: string, page: ReactNode = <Page />) {
  const view = render(
    <MemoryRouter initialEntries={[url]}>{page}</MemoryRouter>
  )
  return { ...view, showSetting: () => fireEvent.click(view.getByText('Load')) }
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 20))

test('lands on the group first, then on the setting once it renders', async () => {
  const { showSetting } = renderAt(
    '/settings/delivery?section=build#setting-docker-compose'
  )
  await waitFor(() => expect(scrolledTo()).toEqual(['settings-section-build']))
  showSetting()
  await waitFor(() =>
    expect(scrolledTo()).toEqual([
      'settings-section-build',
      'setting-docker-compose',
    ])
  )
})

test('stays on the group when this service does not have the setting', async () => {
  const { showSetting } = renderAt(
    '/settings/delivery?section=build#setting-compose-services'
  )
  await waitFor(() => expect(scrolledTo()).toEqual(['settings-section-build']))
  showSetting()
  await settle()
  expect(scrolledTo()).toEqual(['settings-section-build'])
})

/**
 * The delivery page: the linked section is the last one, and the sections
 * above it are still loading, so it moves down once they render.
 */
function DeliveryPage() {
  useScrollToLinkedSection()
  const [loaded, setLoaded] = useState(false)
  // Before: 1,500px of skeletons above a 300px section. After: 2,400px.
  const flagsTop = loaded ? 2400 : 1500
  return (
    <Shell height={flagsTop + 300}>
      <section id="settings-section-previews" data-top={flagsTop - 600}>
        <button type="button" onClick={() => setLoaded(true)}>
          Load
        </button>
        {loaded && <p>Preview settings</p>}
      </section>
      <section id="settings-section-feature-flags" data-top={flagsTop}>
        Feature flags
      </section>
    </Shell>
  )
}

test('the last section scrolls only the page, as far as it goes, never the shell', async () => {
  renderAt('/settings/delivery?section=feature-flags', <DeliveryPage />)
  await waitFor(() =>
    expect(layout.scrolls).toEqual([{ target: 'page', top: 1200 }])
  )
  const frame = document.getElementById('frame') as HTMLElement
  expect(layout.scrollTopOf(frame)).toBe(0)
})

test('keeps the section in view while the content above it loads', async () => {
  const { showSetting } = renderAt(
    '/settings/delivery?section=feature-flags',
    <DeliveryPage />
  )
  await waitFor(() => expect(layout.scrolls).toHaveLength(1))
  showSetting()
  await waitFor(() =>
    expect(layout.scrolls).toEqual([
      { target: 'page', top: 1200 },
      { target: 'page', top: 2100 },
    ])
  )
})

test('once the reader scrolls, the page is theirs', async () => {
  const { showSetting } = renderAt(
    '/settings/delivery?section=feature-flags',
    <DeliveryPage />
  )
  await waitFor(() => expect(layout.scrolls).toHaveLength(1))
  act(() => {
    window.dispatchEvent(new Event('wheel'))
  })
  showSetting()
  await settle()
  expect(layout.scrolls).toEqual([{ target: 'page', top: 1200 }])
})

test('a reader who scrolls before the section renders is not pulled to it', async () => {
  const { showSetting } = renderAt(
    '/settings/delivery?section=build#setting-docker-compose'
  )
  await waitFor(() => expect(scrolledTo()).toEqual(['settings-section-build']))
  act(() => {
    window.dispatchEvent(new Event('touchstart'))
  })
  showSetting()
  await settle()
  expect(scrolledTo()).toEqual(['settings-section-build'])
})

/** A page that changes a query parameter of its own, like a tab. */
function TabbedPage() {
  useScrollToLinkedSection()
  const [, setSearchParams] = useSearchParams()
  return (
    <Shell>
      <section id="settings-section-agents" data-top="400">
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
    </Shell>
  )
}

test('changing another query parameter does not scroll back to the section', async () => {
  const view = renderAt('/settings/automation?section=agents', <TabbedPage />)
  await waitFor(() => expect(scrolledTo()).toEqual(['settings-section-agents']))
  // The reader scrolls away, then switches a tab.
  const page = document.getElementById('page') as HTMLElement
  page.scrollTop = 0
  fireEvent.click(view.getByText('Runs'))
  await settle()
  expect(scrolledTo()).toEqual(['settings-section-agents'])
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
