// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { IssueService } from './GlobalErrors'

installBrowserWindow()
afterEach(cleanup)

test("an issue's service is a link with the service's icon", () => {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <IssueService
          issue={{
            project_id: 7,
            project_name: 'Example API',
            project_slug: 'example-api',
          }}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const link = screen.getByRole('link', { name: 'Example API' })
  expect(link.getAttribute('href')).toBe('/projects/example-api')
  expect(link.querySelector('span[aria-hidden="true"]')).toBeTruthy()
})
