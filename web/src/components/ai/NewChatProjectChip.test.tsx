// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { NewChatProjectChip } from './NewChatProjectChip'

installBrowserWindow()
afterEach(cleanup)

const PROJECTS = [
  {
    id: 1,
    name: 'with-template',
    slug: 'with-template',
    source_type: 'git',
    service_template_image_url: '/presets/example.svg',
  },
  { id: 2, name: 'deployed', slug: 'deployed', source_type: 'git' },
  { id: 3, name: 'empty', slug: 'empty', source_type: 'git' },
]

function openPicker() {
  mockApi({
    'GET /projects': () =>
      jsonResponse({ projects: PROJECTS, total: PROJECTS.length }),
    'GET /deployments/latest-media': () =>
      jsonResponse({ projects: { 2: { url: 'https://app.example.test/' } } }),
  })
  render(
    <QueryClientProvider client={new QueryClient()}>
      <NewChatProjectChip value={null} onChange={() => {}} />
    </QueryClientProvider>
  )
  fireEvent.click(
    screen.getByRole('button', { name: 'Add a project to this chat' })
  )
}

test('the project picker shows each project with its own logo', async () => {
  openPicker()
  // The accessible name stays the project's own: the logo is decoration.
  const withTemplate = await screen.findByRole('button', {
    name: 'with-template with-template',
  })
  expect(withTemplate.querySelector('img')?.getAttribute('src')).toBe(
    '/presets/example.svg'
  )
  const deployed = screen.getByRole('button', { name: 'deployed deployed' })
  await waitFor(() =>
    expect(deployed.querySelector('img')?.getAttribute('src')).toBe(
      'https://app.example.test/favicon.ico'
    )
  )
  const empty = screen.getByRole('button', { name: /empty/ })
  expect(empty.querySelector('img')).toBeNull()
  expect(empty.textContent).toContain('E')
})
