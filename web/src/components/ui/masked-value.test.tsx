// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { MaskedValue } from './masked-value'

installBrowserWindow()
afterEach(cleanup)

test('the reveal toggle is named for what it will do and reports its state', () => {
  const view = render(<MaskedValue value="s3cr3t-token-value" />)

  const show = view.getByRole('button', { name: 'Show value' })
  expect(show.getAttribute('aria-pressed')).toBe('false')
  expect(view.queryByText('s3cr3t-token-value')).toBeNull()

  fireEvent.click(show)

  const hide = view.getByRole('button', { name: 'Hide value' })
  expect(hide.getAttribute('aria-pressed')).toBe('true')
  expect(view.queryByText('s3cr3t-token-value')).not.toBeNull()
})
