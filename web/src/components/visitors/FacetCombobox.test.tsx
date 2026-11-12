// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { FacetCombobox } from './FacetCombobox'

installBrowserWindow()
afterEach(cleanup)

test('a failed facet lookup offers retry instead of claiming there are no values', async () => {
  let retries = 0
  const view = render(
    <FacetCombobox
      label="Country"
      value={undefined}
      options={[]}
      onChange={() => {}}
      error
      onRetry={() => {
        retries++
      }}
    />
  )
  fireEvent.click(view.getByRole('combobox', { name: 'Country filter' }))
  await view.findByText('Could not load country options.')
  expect(view.queryByText('No values in this range.')).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  expect(retries).toBe(1)
})

test('clearing a selected facet is a separate button that does not open its menu', () => {
  let next: string | undefined = 'Exampleland'
  const view = render(
    <FacetCombobox
      label="Country"
      value="Exampleland"
      options={[]}
      onChange={(value) => {
        next = value
      }}
    />
  )
  const clear = view.getByRole('button', { name: 'Clear Country filter' })
  expect(clear.parentElement?.closest('button')).toBeNull()
  fireEvent.click(clear)
  expect(next).toBeUndefined()
  expect(
    view
      .getByRole('combobox', { name: 'Country filter' })
      .getAttribute('aria-expanded')
  ).toBe('false')
})
