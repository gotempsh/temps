// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { Boxes } from 'lucide-react'
import { Tabs, TabsList, TabsTrigger } from './tabs'

installBrowserWindow()
afterEach(cleanup)

// A w-full, overflow-x-auto list inside a flex row only scrolls when its
// parent is allowed to shrink; otherwise the tabs run off the viewport.
test('the tabs root can shrink inside a flex row and keeps caller classes', () => {
  const view = render(
    <Tabs defaultValue="a" className="custom">
      <TabsList>
        <TabsTrigger value="a">A</TabsTrigger>
      </TabsList>
    </Tabs>
  )
  const root = view.container.firstElementChild as HTMLElement
  expect(root.className).toContain('min-w-0')
  expect(root.className).toContain('custom')
})

test('a tab icon sits before the label and stays out of its name', () => {
  const view = render(
    <Tabs defaultValue="a">
      <TabsList>
        <TabsTrigger value="a" icon={Boxes}>
          Containers
        </TabsTrigger>
      </TabsList>
    </Tabs>
  )
  const tab = view.getByRole('tab', { name: 'Containers' })
  const icon = tab.querySelector('svg')
  expect(icon?.getAttribute('aria-hidden')).toBe('true')
  expect(tab.firstElementChild).toBe(icon)
  // Phones keep the label only, so a strip's last tabs stay in view.
  expect(icon?.getAttribute('class')).toContain('max-sm:hidden')
})
