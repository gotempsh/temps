// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import {
  LogAttributeFilter,
  type LogAttributePredicate,
} from './LogAttributeFilter'

installBrowserWindow()
afterEach(cleanup)

test('int64 filter submits exact text, rejects overflow and recovers after correction', async () => {
  const submitted: (LogAttributePredicate | undefined)[] = []
  const view = render(
    <LogAttributeFilter
      value={{
        key: 'order.total',
        value: '9007199254740993',
        type: 'integer',
        operator: 'eq',
      }}
      onApply={(value) => submitted.push(value)}
      pending={false}
    />
  )
  const form = view.getByRole('form', { name: 'Filter log attributes' })
  await act(async () => {
    fireEvent.submit(form)
  })
  expect(submitted[0]?.value).toBe('9007199254740993')
  const value = view.getByLabelText('Attribute value')
  fireEvent.change(value, { target: { value: '9223372036854775808' } })
  await act(async () => {
    fireEvent.submit(form)
  })
  expect(submitted.length).toBe(1)
  expect(view.container.textContent).toContain('Enter a signed 64-bit integer.')
  fireEvent.change(value, { target: { value: '-9223372036854775808' } })
  await act(async () => {
    fireEvent.submit(form)
  })
  expect(submitted[1]?.value).toBe('-9223372036854775808')
  expect(view.container.textContent).not.toContain(
    'Enter a signed 64-bit integer.'
  )
})

test('attribute limits count UTF-8 bytes and clear resets validation and active filtering', async () => {
  const submitted: (LogAttributePredicate | undefined)[] = []
  const view = render(
    <LogAttributeFilter
      value={{
        key: 'synthetic',
        value: 'true',
        type: 'boolean',
        operator: 'eq',
      }}
      onApply={(value) => submitted.push(value)}
      pending={false}
    />
  )
  fireEvent.change(view.getByLabelText('Attribute key'), {
    target: { value: '🚀'.repeat(33) },
  })
  await act(async () => {
    fireEvent.submit(view.getByRole('form', { name: 'Filter log attributes' }))
  })
  expect(submitted.length).toBe(0)
  expect(view.container.textContent).toContain(
    'Enter an attribute key of 1–128 bytes.'
  )
  fireEvent.click(view.getByRole('button', { name: 'Clear attribute filter' }))
  expect(submitted).toEqual([undefined])
  expect((view.getByLabelText('Attribute key') as HTMLInputElement).value).toBe(
    ''
  )
  expect(view.container.textContent).not.toContain(
    'Enter an attribute key of 1–128 bytes.'
  )
})
