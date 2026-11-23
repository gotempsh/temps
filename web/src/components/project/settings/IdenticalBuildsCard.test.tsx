// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import type { IdenticalBuildsResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { IdenticalBuildsCard } from './IdenticalBuildsCard'

installBrowserWindow()
afterEach(cleanup)

const PATH = '/projects/9/identical-builds'

function settings(
  overrides: Partial<IdenticalBuildsResponse> = {}
): IdenticalBuildsResponse {
  return {
    mode: 'parallel',
    scope: 'fingerprint',
    modes: [
      {
        id: 'parallel',
        label: 'Build every time',
        description: 'Every deployment builds its own image.',
        example: '3 projects at the same commit run 3 builds at once.',
      },
      {
        id: 'queue',
        label: 'One at a time',
        description: 'Identical builds wait for each other.',
        example: '3 projects at the same commit build one after another.',
      },
      {
        id: 'share',
        label: 'Build once and share',
        description: 'Identical builds deploy the first build’s image.',
        example:
          '3 projects building the same repository and path at the same commit build once; the other 2 deploy that image.',
      },
    ],
    scopes: [
      {
        id: 'fingerprint',
        label: 'Identical builds only',
        description: 'Same commit, path and build-time values.',
      },
      {
        id: 'source',
        label: 'Same repository and path',
        description: 'Any commit or values.',
      },
    ],
    reuse_window_seconds: 1800,
    max_wait_seconds: 5400,
    activity: { reused: 0, built: 0, waiting: 0, building: 0 },
    can_edit: true,
    ...overrides,
  }
}

function renderCard() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <IdenticalBuildsCard projectId={9} />
    </QueryClientProvider>
  )
}

test('shows every mode with its explanation and example, parallel by default', async () => {
  mockApi({ [`GET ${PATH}`]: () => jsonResponse(settings()) })
  const screen = renderCard()
  const parallel = await screen.findByRole('radio', {
    name: 'Build every time',
  })
  expect(parallel.getAttribute('aria-checked')).toBe('true')
  expect(
    screen.getByText(
      /3 projects building the same repository and path at the same commit build once/
    )
  ).toBeTruthy()
  // The scope only matters once builds coordinate.
  expect(screen.queryByText('Which builds wait')).toBeNull()
  expect(document.getElementById('setting-identical-builds')).not.toBeNull()
})

test('choosing share saves the mode and scope', async () => {
  const bodies: unknown[] = []
  mockApi({
    [`GET ${PATH}`]: () => jsonResponse(settings()),
    [`PUT ${PATH}`]: async (request) => {
      bodies.push(await request.json())
      return jsonResponse(settings({ mode: 'share', scope: 'source' }))
    },
  })
  const screen = renderCard()
  fireEvent.click(
    await screen.findByRole('radio', { name: 'Build once and share' })
  )
  expect(await screen.findByText('Which builds wait')).toBeTruthy()
  fireEvent.click(
    screen.getByRole('radio', { name: 'Same repository and path' })
  )
  fireEvent.click(screen.getByRole('button', { name: 'Save' }))
  await waitFor(() => expect(bodies.length).toBe(1))
  expect(bodies[0]).toEqual({ mode: 'share', scope: 'source' })
  await waitFor(() =>
    expect(
      (screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement)
        .disabled
    ).toBe(true)
  )
})

test('a reader sees the setting but cannot change it', async () => {
  mockApi({
    [`GET ${PATH}`]: () =>
      jsonResponse(settings({ mode: 'queue', can_edit: false })),
  })
  const screen = renderCard()
  const queue = await screen.findByRole('radio', { name: 'One at a time' })
  expect(queue.getAttribute('aria-checked')).toBe('true')
  expect(queue.hasAttribute('disabled')).toBe(true)
  expect(screen.getByText(/You need projects:write/)).toBeTruthy()
  expect(
    (screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled
  ).toBe(true)
})

test('a failed load says why and offers a retry', async () => {
  mockApi({
    [`GET ${PATH}`]: () =>
      jsonResponse(
        {
          title: 'Identical build settings',
          detail: 'Project 9 was not found',
        },
        404
      ),
  })
  const screen = renderCard()
  expect(
    await screen.findByText('Could not load identical builds')
  ).toBeTruthy()
  expect(screen.getByText(/Project 9 was not found/)).toBeTruthy()
})
