// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
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
