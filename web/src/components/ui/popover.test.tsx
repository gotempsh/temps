// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { Popover, PopoverContent, PopoverTrigger } from './popover'

installBrowserWindow()
afterEach(cleanup)

function renderPopover(role?: string) {
  return render(
    <Popover>
      <PopoverTrigger asChild>
        <button type="button" role={role} aria-label="Pick one">
          Pick
        </button>
      </PopoverTrigger>
      <PopoverContent>
        <p>Options</p>
      </PopoverContent>
    </Popover>
  )
}

test.each(['ArrowDown', 'ArrowUp'])(
  '%s opens a collapsed combobox popover',
  async (key) => {
    const view = renderPopover('combobox')
    const trigger = view.getByRole('combobox')
    expect(trigger.getAttribute('aria-expanded')).toBe('false')
    fireEvent.keyDown(trigger, { key })
    expect(trigger.getAttribute('aria-expanded')).toBe('true')
    expect(await view.findByText('Options')).toBeTruthy()
  }
)

test('ArrowDown on an already open combobox does not toggle it closed', async () => {
  const view = renderPopover('combobox')
  const trigger = view.getByRole('combobox')
  fireEvent.keyDown(trigger, { key: 'ArrowDown' })
  fireEvent.keyDown(trigger, { key: 'ArrowDown' })
  expect(trigger.getAttribute('aria-expanded')).toBe('true')
})

test('a plain popover trigger is not opened by ArrowDown', () => {
  const view = renderPopover()
  const trigger = view.getByRole('button')
  fireEvent.keyDown(trigger, { key: 'ArrowDown' })
  expect(trigger.getAttribute('aria-expanded')).toBe('false')
})

test('other keys leave a combobox popover closed', () => {
  const view = renderPopover('combobox')
  const trigger = view.getByRole('combobox')
  fireEvent.keyDown(trigger, { key: 'a' })
  expect(trigger.getAttribute('aria-expanded')).toBe('false')
})
