// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { installBrowserWindow } from '@/test/dom'
import { moveWindows } from '@/lib/refresh'
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { useWindowQuery } from './useWindowQuery'

installBrowserWindow()
afterEach(cleanup)

function deferred() {
  let resolve!: (value: string) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<string>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

function renderBreakdown(
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
) {
  const answers: Array<ReturnType<typeof deferred>> = []
  function Breakdown() {
    const [start, setStart] = useState(0)
    const [end, setEnd] = useState(10)
    const [level, setLevel] = useState('country')
    const [, setRenders] = useState(0)
    const query = useWindowQuery({
      queryKey: [
        'breakdown',
        { start_date: start, end_date: end, group_by: level },
      ],
      queryFn: () => {
        const answer = deferred()
        answers.push(answer)
        return answer.promise
      },
    })
    return (
      <>
        <button
          type="button"
          onClick={() =>
            moveWindows(() => {
              setStart((n) => n + 1)
              setEnd((n) => n + 1)
            })
          }
        >
          Move window
        </button>
        <button
          type="button"
          onClick={() => {
            setStart((n) => n + 1)
            setEnd((n) => n + 1)
          }}
        >
          Pick the next range
        </button>
        <button type="button" onClick={() => setStart(-20)}>
          Pick a wider range
        </button>
        <button type="button" onClick={() => setLevel('region')}>
          Drill down
        </button>
        <button type="button" onClick={() => setRenders((n) => n + 1)}>
          Render again
        </button>
        <p>
          {query.isPending ? 'loading' : query.isError ? 'failed' : query.data}
        </p>
      </>
    )
  }
  render(
    <QueryClientProvider client={client}>
      <Breakdown />
    </QueryClientProvider>
  )
  return answers
}

test('a moved window keeps the current rows on screen while it loads', async () => {
  const answers = renderBreakdown()
  await act(async () => answers[0].resolve('countries'))
  await screen.findByText('countries')

  fireEvent.click(screen.getByRole('button', { name: 'Move window' }))

  expect(answers).toHaveLength(2)
  expect(screen.getByText('countries')).toBeTruthy()
  await act(async () => answers[1].resolve('countries, later'))
  await screen.findByText('countries, later')
})

test('a drill-down shows loading, never the previous level under the new one', async () => {
  const answers = renderBreakdown()
  await act(async () => answers[0].resolve('countries'))
  await screen.findByText('countries')

  fireEvent.click(screen.getByRole('button', { name: 'Drill down' }))

  await screen.findByText('loading')
  expect(screen.queryByText('countries')).toBeNull()
  await act(async () => answers[1].resolve('regions'))
  await screen.findByText('regions')
})

test('a moved window that fails keeps the last loaded rows, and the shell says the latest did not load', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const answers = renderBreakdown(client)
  await act(async () => answers[0].resolve('countries'))
  await screen.findByText('countries')

  fireEvent.click(screen.getByRole('button', { name: 'Move window' }))
  const error = { status: 500, title: 'Internal Server Error' }
  await act(async () => answers[1].reject(error))

  await act(async () => new Promise((resolve) => setTimeout(resolve, 5)))
  expect(screen.getByText('countries')).toBeTruthy()
  expect(screen.queryByText('failed')).toBeNull()
  expect(collectRouteQueryFailures(client)).toEqual([
    {
      queryHash: expect.any(String),
      error,
      retained: true,
    },
  ])
})

test('a range picked next to the one on screen loads from scratch, never under the old rows', async () => {
  const answers = renderBreakdown()
  await act(async () => answers[0].resolve('countries'))
  await screen.findByText('countries')

  fireEvent.click(screen.getByRole('button', { name: 'Pick the next range' }))

  await screen.findByText('loading')
  expect(screen.queryByText('countries')).toBeNull()
  await act(async () =>
    answers[1].reject({ status: 500, title: 'Internal Server Error' })
  )
  await screen.findByText('failed')
  expect(screen.queryByText('countries')).toBeNull()
})

test('a window moved while another range loads keeps nothing from before it', async () => {
  const answers = renderBreakdown()
  await act(async () => answers[0].resolve('countries'))
  await screen.findByText('countries')

  fireEvent.click(screen.getByRole('button', { name: 'Pick a wider range' }))
  await screen.findByText('loading')
  fireEvent.click(screen.getByRole('button', { name: 'Move window' }))

  expect(screen.getByText('loading')).toBeTruthy()
  expect(screen.queryByText('countries')).toBeNull()
})

test('another range that fails shows the error, not the previous range', async () => {
  const answers = renderBreakdown()
  await act(async () => answers[0].resolve('countries'))
  await screen.findByText('countries')

  fireEvent.click(screen.getByRole('button', { name: 'Pick a wider range' }))
  await act(async () =>
    answers[1].reject({ status: 500, title: 'Internal Server Error' })
  )

  await screen.findByText('failed')
  expect(screen.queryByText('countries')).toBeNull()
})

test('the kept rows stay after the cache drops the window they came from', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const answers = renderBreakdown(client)
  await act(async () => answers[0].resolve('countries'))
  await screen.findByText('countries')

  fireEvent.click(screen.getByRole('button', { name: 'Move window' }))
  await act(async () =>
    answers[1].reject({ status: 500, title: 'Internal Server Error' })
  )
  await act(async () => new Promise((resolve) => setTimeout(resolve, 5)))
  expect(screen.getByText('countries')).toBeTruthy()

  // What `gcTime` does to the inactive earlier window after five minutes.
  const [earlier] = client
    .getQueryCache()
    .findAll({ predicate: (query) => query.state.data !== undefined })
  act(() => client.getQueryCache().remove(earlier))
  fireEvent.click(screen.getByRole('button', { name: 'Render again' }))

  expect(screen.getByText('countries')).toBeTruthy()
  expect(screen.queryByText('failed')).toBeNull()

  // The next Refresh moves the failed window again, and that one fails too.
  fireEvent.click(screen.getByRole('button', { name: 'Move window' }))
  expect(screen.getByText('countries')).toBeTruthy()
  await act(async () =>
    answers[2].reject({ status: 500, title: 'Internal Server Error' })
  )
  await act(async () => new Promise((resolve) => setTimeout(resolve, 5)))
  expect(screen.getByText('countries')).toBeTruthy()
  expect(screen.queryByText('failed')).toBeNull()

  // A window that loads replaces the kept rows.
  fireEvent.click(screen.getByRole('button', { name: 'Move window' }))
  await act(async () => answers[3].resolve('countries, later'))
  await screen.findByText('countries, later')
})
