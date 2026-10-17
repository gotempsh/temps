// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from './select'

installBrowserWindow()
afterEach(cleanup)

function renderSelect(trigger: React.ReactNode) {
  return render(
    <Select defaultValue="all">
      {trigger}
      <SelectContent>
        <SelectItem value="all">All environments</SelectItem>
        <SelectItem value="prod">Production</SelectItem>
      </SelectContent>
    </Select>
  )
}

// The combobox role takes its accessible name from the author, never from its
// content, so a trigger nobody labelled would otherwise be unnamed.
test('an unlabelled trigger is named by the value it shows', async () => {
  const view = renderSelect(
    <SelectTrigger>
      <SelectValue />
    </SelectTrigger>
  )
  const trigger = await view.findByRole('combobox')
  const labelledBy = trigger.getAttribute('aria-labelledby')
  expect(labelledBy).toBeTruthy()
  // Not a reference to itself: axe ignores self-references.
  expect(labelledBy).not.toBe(trigger.id)
  const named = trigger.ownerDocument.getElementById(labelledBy!)
  expect(named?.textContent).toBe('All environments')
  expect(trigger.hasAttribute('aria-label')).toBe(false)
})

test('the fallback name follows the selected value', async () => {
  const view = render(
    <Select defaultValue="all">
      <SelectTrigger>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="all">All environments</SelectItem>
        <SelectItem value="prod">Production</SelectItem>
      </SelectContent>
    </Select>
  )
  const trigger = await view.findByRole('combobox')
  const named = () =>
    trigger.ownerDocument.getElementById(
      trigger.getAttribute('aria-labelledby')!
    )?.textContent
  expect(named()).toBe('All environments')
  view.rerender(
    <Select value="prod">
      <SelectTrigger>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="all">All environments</SelectItem>
        <SelectItem value="prod">Production</SelectItem>
      </SelectContent>
    </Select>
  )
  expect(named()).toBe('Production')
})

test('an explicit aria-label is kept and not replaced', async () => {
  const view = renderSelect(
    <SelectTrigger aria-label="Environment">
      <SelectValue />
    </SelectTrigger>
  )
  const trigger = await view.findByRole('combobox')
  expect(trigger.getAttribute('aria-label')).toBe('Environment')
  expect(trigger.hasAttribute('aria-labelledby')).toBe(false)
})

test('an explicit aria-labelledby is kept', async () => {
  const view = render(
    <>
      <span id="env-heading">Environment</span>
      <Select defaultValue="all">
        <SelectTrigger aria-labelledby="env-heading">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="all">All environments</SelectItem>
        </SelectContent>
      </Select>
    </>
  )
  const trigger = await view.findByRole('combobox')
  expect(trigger.getAttribute('aria-labelledby')).toBe('env-heading')
})

test('a trigger with an associated <label> is left to that label', async () => {
  const view = render(
    <>
      <label htmlFor="env-select">Environment</label>
      <Select defaultValue="all">
        <SelectTrigger id="env-select">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="all">All environments</SelectItem>
        </SelectContent>
      </Select>
    </>
  )
  const trigger = await view.findByRole('combobox')
  expect(trigger.id).toBe('env-select')
  expect(trigger.hasAttribute('aria-labelledby')).toBe(false)
})

test('forwards its ref to the button', () => {
  let node: HTMLButtonElement | null = null
  render(
    <Select defaultValue="all">
      <SelectTrigger
        aria-label="Environment"
        ref={(element) => {
          node = element
        }}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="all">All environments</SelectItem>
      </SelectContent>
    </Select>
  )
  expect(node).not.toBeNull()
})
