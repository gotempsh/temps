// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, useLocation } from 'react-router'
import { useUrlState } from '@temps-sdk/ds'
import { ListToolbar } from './ListToolbar'

import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'

installBrowserWindow()
afterEach(cleanup)

function UrlBoundToolbar() {
  const { get, patch } = useUrlState<'q' | 'page'>()
  const location = useLocation()
  return (
    <div>
      <ListToolbar
        searchLabel="Filter projects"
        placeholder="Filter projects…"
        value={get('q') ?? ''}
        onChange={(value) => patch({ q: value || undefined, page: undefined })}
      />
      <output data-testid="location">{location.search}</output>
    </div>
  )
}

test('typing into a URL-bound toolbar keeps every character and resets the page', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/projects?page=4']}>
      <UrlBoundToolbar />
    </MemoryRouter>
  )
  const input = view.getByLabelText('Filter projects') as HTMLInputElement
  let typed = ''
  for (const char of 'qa-ui-qj14a') {
    typed += char
    fireEvent.change(input, { target: { value: typed } })
  }
  expect(input.value).toBe('qa-ui-qj14a')
  await waitFor(() => {
    const params = new URLSearchParams(
      view.getByTestId('location').textContent ?? ''
    )
    expect(params.get('q')).toBe('qa-ui-qj14a')
    expect(params.has('page')).toBe(false)
  })
  expect(input.value).toBe('qa-ui-qj14a')
  view.unmount()
})

test('the clear button empties both the box and the URL', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/projects?q=abc']}>
      <UrlBoundToolbar />
    </MemoryRouter>
  )
  await act(async () => {
    fireEvent.click(view.getByLabelText('Clear filter projects'))
  })
  expect(
    (view.getByLabelText('Filter projects') as HTMLInputElement).value
  ).toBe('')
  expect(view.getByTestId('location').textContent).toBe('')
  view.unmount()
})
