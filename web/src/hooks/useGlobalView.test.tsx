// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { useGlobalView } from './useGlobalView'

installBrowserWindow()
afterEach(cleanup)

function at(route: string) {
  return function Router({ children }: { children: ReactNode }) {
    return <MemoryRouter initialEntries={[route]}>{children}</MemoryRouter>
  }
}

test('advance moves a relative window to now and keeps the page and filters', async () => {
  const { result } = renderHook(() => useGlobalView(), {
    wrapper: at('/errors?range=1h&page=3&q=checkout&project_id=7'),
  })
  const before = result.current.to
  await new Promise((resolve) => setTimeout(resolve, 5))

  act(() => result.current.advance())

  expect(Date.parse(result.current.to)).toBeGreaterThan(Date.parse(before))
  expect(result.current.range).toBe('1h')
  expect(result.current.page).toBe(3)
  expect(result.current.search).toBe('checkout')
  expect(result.current.projectId).toBe(7)
})

test('advance leaves a custom window where it is', () => {
  const from = '2026-10-01T00:00:00.000Z'
  const to = '2026-10-01T06:00:00.000Z'
  const { result } = renderHook(() => useGlobalView(), {
    wrapper: at(`/errors?range=custom&from=${from}&to=${to}&page=2`),
  })

  act(() => result.current.advance())

  expect(result.current.from).toBe(from)
  expect(result.current.to).toBe(to)
  expect(result.current.page).toBe(2)
})
