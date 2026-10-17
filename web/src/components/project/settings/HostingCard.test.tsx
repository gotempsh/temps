// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { Hosting, ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { HostingCard } from './HostingCard'

installBrowserWindow()
afterEach(cleanup)

function card(hosting: Hosting) {
  const project = {
    id: 7,
    slug: 'shop',
    name: 'Shop',
    hosting,
  } as ProjectResponse
  return (
    <MemoryRouter>
      <HostingCard project={project} refetch={() => {}} />
    </MemoryRouter>
  )
}

test('follows a hosting change made elsewhere instead of offering to save it', async () => {
  const view = render(card('hosted'))
  expect(view.getByText('Temps hosts this app.')).not.toBeNull()

  // The project is marked external elsewhere (another tab, a refetch).
  await act(async () => {
    view.rerender(card('external'))
  })
  expect(view.getByText('This app runs outside Temps.')).not.toBeNull()
})

test('an unsaved choice shows the save controls instead of the status line', async () => {
  const view = render(card('hosted'))
  await act(async () => {
    fireEvent.click(view.getByText('It runs elsewhere'))
  })
  expect(view.queryByText('Temps hosts this app.')).toBeNull()
})
