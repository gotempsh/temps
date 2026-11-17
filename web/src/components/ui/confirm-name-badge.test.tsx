// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { ConfirmNameBadge } from './confirm-name-badge'

installBrowserWindow()

const originalClipboard = Object.getOwnPropertyDescriptor(
  globalThis.navigator,
  'clipboard'
)
afterEach(() => {
  cleanup()
  if (originalClipboard)
    Object.defineProperty(globalThis.navigator, 'clipboard', originalClipboard)
  else Reflect.deleteProperty(globalThis.navigator, 'clipboard')
})

test('copies the exact name the user must type and shows it was copied', async () => {
  const writeText = mock(async (_value: string) => undefined)
  Object.defineProperty(globalThis.navigator, 'clipboard', {
    configurable: true,
    value: { writeText },
  })
  const { getByRole } = render(<ConfirmNameBadge value="orders-db" />)

  const button = getByRole('button', { name: 'Copy "orders-db"' })
  expect(button.textContent).toBe('orders-db')
  fireEvent.click(button)

  await waitFor(() => expect(writeText).toHaveBeenCalledWith('orders-db'))
  await waitFor(() =>
    expect(button.querySelector('svg')?.getAttribute('class')).toContain(
      'lucide-check'
    )
  )
})
