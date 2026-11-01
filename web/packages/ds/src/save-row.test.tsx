// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { SaveRow } from './save-row'

afterEach(cleanup)

const save = (view: ReturnType<typeof render>) =>
  view.getByRole('button', { name: 'Save' }) as HTMLButtonElement

test('Save stays disabled until something changed, and says so', () => {
  const view = render(<SaveRow dirty={false} />)
  expect(save(view).disabled).toBe(true)
  expect(view.getByText('All changes saved')).not.toBeNull()

  view.rerender(<SaveRow dirty />)
  expect(save(view).disabled).toBe(false)
  expect(view.getByText('You have unsaved changes')).not.toBeNull()
})

test('a hint replaces the default status, and another blocker disables Save', () => {
  const view = render(<SaveRow dirty disabled hint="Applies to new builds." />)
  expect(view.getByText('Applies to new builds.')).not.toBeNull()
  expect(save(view).disabled).toBe(true)
})

test('with onSave it is a button, not a form submit', () => {
  const onSave = mock(() => {})
  const view = render(<SaveRow dirty onSave={onSave} />)
  expect(save(view).getAttribute('type')).toBe('button')
  fireEvent.click(save(view))
  expect(onSave).toHaveBeenCalledTimes(1)
})
