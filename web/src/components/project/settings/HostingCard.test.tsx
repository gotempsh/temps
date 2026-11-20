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

function saveButton(view: ReturnType<typeof render>) {
  return view.getByRole('button', { name: 'Save' }) as HTMLButtonElement
}

function radio(view: ReturnType<typeof render>, name: string) {
  return view.getByRole('radio', { name }) as HTMLButtonElement
}

test('follows a hosting change made elsewhere instead of offering to save it', async () => {
  const view = render(card('hosted'))
  expect(radio(view, 'Temps runs it').getAttribute('aria-checked')).toBe('true')
  expect(saveButton(view).disabled).toBe(true)

  // The project is marked external elsewhere (another tab, a refetch).
  await act(async () => {
    view.rerender(card('external'))
  })
  expect(radio(view, 'It runs elsewhere').getAttribute('aria-checked')).toBe(
    'true'
  )
  expect(saveButton(view).disabled).toBe(true)
})

test('an unsaved choice enables Save and says what saving it does', async () => {
  const view = render(card('hosted'))
  await act(async () => {
    fireEvent.click(view.getByText('It runs elsewhere'))
  })
  expect(saveButton(view).disabled).toBe(false)
  expect(
    view.getByText(/Running deployments keep running; Temps refuses new ones/)
  ).not.toBeNull()
})
