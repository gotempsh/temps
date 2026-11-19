// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { ResponsivePagination } from './responsive-pagination'

installBrowserWindow()
afterEach(cleanup)

/// A pager inside a form (a picker on a settings page) only changes the page:
/// going to a page must never submit, and so save, the form around it.
function renderInsideForm() {
  const saved = mock(() => {})
  const pageChanged = mock((_page: number) => {})
  const view = render(
    <form
      onSubmit={(event) => {
        event.preventDefault()
        saved()
      }}
    >
      <ResponsivePagination
        page={1}
        pageSize={20}
        total={100}
        totalPages={5}
        onPageChange={pageChanged}
      />
    </form>
  )
  return { saved, pageChanged, view }
}

test('going to a page from the page box does not submit the surrounding form', () => {
  const { saved, pageChanged } = renderInsideForm()
  const box = screen.getByRole('spinbutton', { name: 'Page number' })
  fireEvent.change(box, { target: { value: '4' } })
  fireEvent.click(screen.getByRole('button', { name: 'Go' }))
  expect(pageChanged).toHaveBeenCalledWith(4)
  expect(saved).not.toHaveBeenCalled()
})

test('pressing Enter in the page box goes to the page without submitting the form', () => {
  const { saved, pageChanged } = renderInsideForm()
  const box = screen.getByRole('spinbutton', { name: 'Page number' })
  fireEvent.change(box, { target: { value: '9' } })
  const enter = fireEvent.keyDown(box, { key: 'Enter' })
  // Clamped to the last page; the key's default (implicit submission of the
  // surrounding form) is cancelled.
  expect(pageChanged).toHaveBeenCalledWith(5)
  expect(enter).toBe(false)
  expect(saved).not.toHaveBeenCalled()
})

test('the pager renders no form of its own, so it never nests one', () => {
  const { view } = renderInsideForm()
  expect(view.container.querySelectorAll('form')).toHaveLength(1)
})
