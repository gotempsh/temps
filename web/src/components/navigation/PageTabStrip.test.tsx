// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { Circle } from 'lucide-react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { PageTabStrip, type PageTab } from './PageTabStrip'

installBrowserWindow()
afterEach(cleanup)

const tab = (title: string, active = false): PageTab => ({
  key: title,
  title,
  icon: Circle,
  to: `/${title.toLowerCase()}`,
  active,
})

const titles = (nav: HTMLElement) =>
  [...nav.querySelectorAll('a')].map((a) => a.textContent)

const open = (trigger: HTMLElement) =>
  fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })

function renderStrip(tabs: PageTab[]) {
  return render(
    <MemoryRouter>
      <PageTabStrip label="Area pages" tabs={tabs} />
    </MemoryRouter>
  )
}

/**
 * Lays the strip out as a browser would: a `nav` of `stripWidth`, each tab
 * 80px wide. Restored by descriptor, so no own copy is left on the
 * prototype to hide another test's fake.
 */
function withLayout(stripWidth: number, run: () => void) {
  const rect = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    'getBoundingClientRect'
  )
  const client = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    'clientWidth'
  )
  HTMLElement.prototype.getBoundingClientRect = function () {
    return { left: 0, right: 80, width: 80 } as DOMRect
  }
  Object.defineProperty(HTMLElement.prototype, 'clientWidth', {
    configurable: true,
    get(this: HTMLElement) {
      return this.tagName === 'NAV' ? stripWidth : 0
    },
  })
  try {
    run()
  } finally {
    if (rect)
      Object.defineProperty(
        HTMLElement.prototype,
        'getBoundingClientRect',
        rect
      )
    else Reflect.deleteProperty(HTMLElement.prototype, 'getBoundingClientRect')
    if (client)
      Object.defineProperty(HTMLElement.prototype, 'clientWidth', client)
    else Reflect.deleteProperty(HTMLElement.prototype, 'clientWidth')
  }
}

test('one page renders no strip', () => {
  const view = renderStrip([tab('Alpha', true)])
  expect(view.queryByRole('navigation')).toBeNull()
})

test('the tabs are links, the one shown marked', () => {
  const view = renderStrip([tab('Alpha'), tab('Beta', true), tab('Gamma')])
  const nav = view.getByRole('navigation', { name: 'Area pages' })
  expect(titles(nav)).toEqual(['Alpha', 'Beta', 'Gamma'])
  expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe('Beta')
  expect(view.queryByRole('button', { name: 'More Area pages' })).toBeNull()
})

test('the strip never scrolls sideways or wraps', () => {
  const view = renderStrip([tab('Alpha'), tab('Beta', true)])
  const nav = view.getByRole('navigation', { name: 'Area pages' })
  expect(nav.className).toContain('overflow-hidden')
  expect(nav.className).not.toContain('overflow-x-auto')
  expect(nav.className).not.toContain('flex-wrap')
})

test('a tab that holds several pages opens a menu of them', async () => {
  const view = renderStrip([
    tab('Alpha'),
    {
      ...tab('Beta', true),
      pages: [
        { key: 'one', title: 'Beta', icon: Circle, to: '/beta', active: false },
        {
          key: 'two',
          title: 'Second',
          icon: Circle,
          to: '/beta/second',
          active: true,
        },
      ],
    },
  ])
  const trigger = view.getByRole('button', { name: /^Beta/ })
  // It says which of its pages is shown.
  expect(trigger.textContent).toBe('BetaSecond')
  expect(trigger.getAttribute('aria-current')).toBe('true')
  open(trigger)
  const menu = await view.findByRole('menu')
  expect(
    [...menu.querySelectorAll('a')].map((a) => a.getAttribute('href'))
  ).toEqual(['/beta', '/beta/second'])
  expect(menu.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Second'
  )
  // The entries of a menu lead with an icon like the tabs do.
  for (const link of menu.querySelectorAll('a'))
    expect(link.firstElementChild?.tagName.toLowerCase()).toBe('svg')
})

test('every tab leads with an icon, a tab that opens a menu included', () => {
  const view = renderStrip([
    tab('Alpha'),
    {
      ...tab('Beta', true),
      pages: [
        { key: 'one', title: 'Beta', icon: Circle, to: '/beta', active: true },
        { key: 'two', title: 'Two', icon: Circle, to: '/two', active: false },
      ],
    },
  ])
  const nav = view.getByRole('navigation', { name: 'Area pages' })
  const tabs = [...nav.querySelectorAll('[data-tab]')]
  expect(tabs).toHaveLength(2)
  for (const item of tabs) {
    const icon = item.firstElementChild
    expect(icon?.tagName.toLowerCase()).toBe('svg')
    expect(icon?.getAttribute('aria-hidden')).toBe('true')
    // No width at which it is dropped.
    expect(icon?.getAttribute('class')).not.toContain('hidden')
  }
})

test('tabs that do not fit are listed under More', async () => {
  // Four 80px tabs need 320px; 200px holds one beside More.
  await new Promise<void>((done) => {
    withLayout(200, () => {
      const view = renderStrip([
        tab('Alpha'),
        tab('Beta'),
        tab('Gamma'),
        tab('Delta', true),
      ])
      const nav = view.getByRole('navigation', { name: 'Area pages' })
      // The tab shown is never one of the hidden ones.
      expect(titles(nav)).toEqual(['Delta'])
      open(view.getByRole('button', { name: 'More Area pages' }))
      void view.findByRole('menu').then((menu) => {
        expect(
          [...menu.querySelectorAll('a')].map((a) => a.getAttribute('href'))
        ).toEqual(['/alpha', '/beta', '/gamma'])
        done()
      })
    })
  })
})

test('a strip with room shows every tab and no More', () => {
  withLayout(400, () => {
    const view = renderStrip([tab('Alpha'), tab('Beta'), tab('Gamma', true)])
    const nav = view.getByRole('navigation', { name: 'Area pages' })
    expect(titles(nav)).toEqual(['Alpha', 'Beta', 'Gamma'])
    expect(view.queryByRole('button', { name: 'More Area pages' })).toBeNull()
  })
})
