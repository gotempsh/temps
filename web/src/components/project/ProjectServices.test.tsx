// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { ProjectServices } from './ProjectServices'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 1, slug: 'web' } as ProjectResponse

function Where() {
  const { pathname, search, hash } = useLocation()
  return <output data-testid="where">{`${pathname}${search}${hash}`}</output>
}

function renderAt(entry: string) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <Routes>
        <Route
          path="/projects/web/services/*"
          element={<ProjectServices project={project} />}
        />
        <Route path="/projects/web/storage" element={<Where />} />
      </Routes>
    </MemoryRouter>
  )
}

test('the services index keeps the query string and hash', () => {
  const view = renderAt('/projects/web/services?a=b#c')
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/storage?a=b#c'
  )
})

test('an unknown services path keeps the query string and hash', () => {
  const view = renderAt('/projects/web/services/legacy?a=b#c')
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/storage?a=b#c'
  )
})
