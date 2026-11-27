// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { BuilderLimitsPanel } from './BuilderLimitsPanel'

afterEach(cleanup)

test('shows the form at once, saves per-node limits and retains edits when the server rejects them', async () => {
  let submitted: unknown
  mockApi({
    'PUT /internal/nodes/7/build-limits': async (request) => {
      submitted = await request.json()
      return jsonResponse({ detail: 'Worker 7 is unavailable' }, 409)
    },
  })
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  const node = {
    id: 7,
    build_limits: {
      concurrency: 1,
      queue_wait_seconds: 300,
      build_timeout_seconds: 1800,
    },
    build_limits_revision: '4',
  } as NodeInfoResponse
  const view = render(
    <QueryClientProvider client={client}>
      <BuilderLimitsPanel node={node} />
    </QueryClientProvider>
  )
  const concurrency = view.getByLabelText(
    'Concurrent builds'
  ) as HTMLInputElement
  fireEvent.change(concurrency, { target: { value: '3' } })
  fireEvent.submit(
    view.getByRole('button', { name: 'Save builder limits' }).closest('form')!
  )
  await waitFor(() =>
    expect(view.getByRole('alert').textContent).toContain(
      'Worker 7 is unavailable'
    )
  )
  expect(submitted).toEqual({
    concurrency: 3,
    queue_wait_seconds: 300,
    build_timeout_seconds: 1800,
    expected_revision: '4',
  })
  expect(concurrency.value).toBe('3')

  fireEvent.click(view.getByRole('button', { name: 'Reset' }))
  await waitFor(() => expect(concurrency.value).toBe('1'))
  expect(view.queryByRole('alert')).toBeNull()
  expect(
    (
      view.getByRole('button', {
        name: 'Save builder limits',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)
  client.clear()
})

test('a save refused because someone else saved since says so and loads their limits', async () => {
  mockApi({
    'PUT /internal/nodes/7/build-limits': () =>
      jsonResponse(
        {
          title: 'Settings Changed Since They Were Read',
          detail: 'Build limits of node 7 changed since it was read',
          expected_revision: '4',
          current_revision: '5',
        },
        409
      ),
  })
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  const read = {
    id: 7,
    build_limits: {
      concurrency: 1,
      queue_wait_seconds: 300,
      build_timeout_seconds: 1800,
    },
    build_limits_revision: '4',
  } as NodeInfoResponse
  const view = render(
    <QueryClientProvider client={client}>
      <BuilderLimitsPanel node={read} />
    </QueryClientProvider>
  )
  const concurrency = view.getByLabelText(
    'Concurrent builds'
  ) as HTMLInputElement
  fireEvent.change(concurrency, { target: { value: '3' } })
  fireEvent.submit(
    view.getByRole('button', { name: 'Save builder limits' }).closest('form')!
  )
  await waitFor(() =>
    expect(view.getByRole('alert').textContent).toContain(
      'Someone else saved these settings'
    )
  )
  expect(concurrency.value).toBe('3')

  // The node refetched after the refusal carries the other admin's save.
  view.rerender(
    <QueryClientProvider client={client}>
      <BuilderLimitsPanel
        node={
          {
            ...read,
            build_limits: { ...read.build_limits, concurrency: 4 },
            build_limits_revision: '5',
          } as NodeInfoResponse
        }
      />
    </QueryClientProvider>
  )
  fireEvent.click(view.getByRole('button', { name: 'Load latest' }))
  await waitFor(() => expect(concurrency.value).toBe('4'))
  expect(view.queryByRole('alert')).toBeNull()
  client.clear()
})

test('a save sends the revision the form was loaded with, not one a background refetch brought', async () => {
  const submitted: unknown[] = []
  mockApi({
    'PUT /internal/nodes/7/build-limits': async (request) => {
      const body = (await request.json()) as Record<string, number>
      submitted.push(body)
      return jsonResponse({
        node_id: 7,
        concurrency: body.concurrency,
        queue_wait_seconds: body.queue_wait_seconds,
        build_timeout_seconds: body.build_timeout_seconds,
        is_default: false,
        revision: '6',
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  const read = {
    id: 7,
    build_limits: {
      concurrency: 1,
      queue_wait_seconds: 300,
      build_timeout_seconds: 1800,
    },
    build_limits_revision: '4',
  } as NodeInfoResponse
  const panel = (node: NodeInfoResponse) => (
    <QueryClientProvider client={client}>
      <BuilderLimitsPanel node={node} />
    </QueryClientProvider>
  )
  const view = render(panel(read))
  // Another admin saved; the node refetched while this form shows revision 4.
  view.rerender(
    panel({
      ...read,
      build_limits: { ...read.build_limits, concurrency: 4 },
      build_limits_revision: '5',
    } as NodeInfoResponse)
  )
  const concurrency = view.getByLabelText(
    'Concurrent builds'
  ) as HTMLInputElement
  fireEvent.change(concurrency, { target: { value: '3' } })
  const form = view
    .getByRole('button', { name: 'Save builder limits' })
    .closest('form')!
  fireEvent.submit(form)
  await waitFor(() => expect(submitted).toHaveLength(1))
  expect(submitted[0]).toMatchObject({ expected_revision: '4' })

  // After a save, the next one is based on the revision that save returned.
  await waitFor(() =>
    expect(
      (
        view.getByRole('button', {
          name: 'Save builder limits',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
  )
  fireEvent.change(concurrency, { target: { value: '2' } })
  fireEvent.submit(form)
  await waitFor(() => expect(submitted).toHaveLength(2))
  expect(submitted[1]).toMatchObject({ expected_revision: '6' })
  client.clear()
})
