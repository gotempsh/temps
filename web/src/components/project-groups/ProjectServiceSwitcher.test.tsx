// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { ProjectServiceSwitcher } from './ProjectServiceSwitcher'

installBrowserWindow()
afterEach(cleanup)

const service = (id: number, slug: string) =>
  ({ id, slug, name: slug, preset: 'dockerfile' }) as unknown as ProjectResponse

test('a service row shows its type as an icon, leaving the width to its name', async () => {
  const api = service(1, 'example-api')
  const migrations = service(2, 'example-postgres-migrations')
  const view = render(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <ProjectServiceSwitcher
          project={api}
          group={
            {
              id: 5,
              slug: 'example',
              name: 'Example',
              projects: [api, migrations],
            } as unknown as ProjectGroupResponse
          }
        >
          example-api
        </ProjectServiceSwitcher>
      </MemoryRouter>
    </QueryClientProvider>
  )

  fireEvent.click(view.getByRole('button', { name: /switch to another/ }))

  const row = (await view.findByText('example-postgres-migrations')).closest(
    '[role="option"]'
  )
  expect(row?.textContent).not.toContain('Dockerfile')
  // The type is still named for anyone who hovers the icon.
  expect(row?.querySelector('[title="Dockerfile"]')).not.toBeNull()
})
