// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectDsnResponse, ProjectResponse } from '@/api/client'
import { listDsnsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ingestSetupFixture } from '@/lib/ingest-setup.fixture'
import { installBrowserWindow } from '@/test/dom'
import { ErrorTrackingSetup } from './ErrorTrackingSetup'
import { ingestSetupClient } from './ingest-setup-cache.test-helpers'

installBrowserWindow()
afterEach(cleanup)

test('the plain HTTP option shows the DSN its examples read from $SENTRY_DSN', () => {
  const dsn = 'https://0123456789abcdef@temps.example.com/42'
  const client = ingestSetupClient({
    kind: 'loaded',
    data: ingestSetupFixture(),
  })
  client.setQueryData(listDsnsOptions({ path: { project_id: 42 } }).queryKey, [
    { dsn } as ProjectDsnResponse,
  ])

  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ErrorTrackingSetup
          project={
            { id: 42, name: 'My app', slug: 'my-app' } as ProjectResponse
          }
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  fireEvent.click(view.getByText('Plain HTTP / any framework'))
  fireEvent.click(view.getByRole('button', { name: 'Continue' }))

  expect(view.getByTestId('plain-http-errors')).toBeTruthy()
  expect(view.container.textContent).toContain(`SENTRY_DSN=${dsn}`)
})
