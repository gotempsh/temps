// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import axe from 'axe-core'
import { installBrowserWindow } from '@/test/dom'
import { SearchableSelect } from './searchable-select'
import { Switch } from './switch'

installBrowserWindow()
afterEach(cleanup)

// Rules that do not depend on layout or computed colour, so they are reliable
// without a real browser. Contrast and scroll regions are covered by the QA
// catalog's accessibility sweep.
const RULES = [
  'button-name',
  'aria-valid-attr-value',
  'aria-allowed-attr',
  'aria-prohibited-attr',
  'aria-required-attr',
  'nested-interactive',
  'label',
]

async function violations(container: HTMLElement) {
  const result = await axe.run(container, {
    runOnly: { type: 'rule', values: RULES },
  })
  return result.violations.map((v) => `${v.id}: ${v.nodes.length}`)
}

const OPTIONS = [
  { value: 'all', label: 'All projects' },
  { value: 'web', label: 'Web app' },
]

test('a popover combobox has no structural violations, closed or open', async () => {
  const view = render(
    <SearchableSelect
      label="Project"
      value="all"
      onValueChange={() => {}}
      options={OPTIONS}
    />
  )
  expect(await violations(view.container)).toEqual([])

  const trigger = view.getByRole('combobox')
  fireEvent.keyDown(trigger, { key: 'ArrowDown' })
  expect(trigger.getAttribute('aria-expanded')).toBe('true')
  expect(await violations(view.baseElement)).toEqual([])
})

test('a labelled switch inside a form has no structural violations', async () => {
  const view = render(
    <form>
      <label htmlFor="enabled">Enabled</label>
      <Switch id="enabled" defaultChecked />
    </form>
  )
  expect(await violations(view.container)).toEqual([])
})
