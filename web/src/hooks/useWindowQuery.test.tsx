// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { installBrowserWindow } from '@/test/dom'
import { useWindowQuery } from './useWindowQuery'

installBrowserWindow()
afterEach(cleanup)

function deferred() {
  let resolve!: (value: string) => void
  const promise = new Promise<string>((res) => (resolve = res))
  return { promise, resolve }
}

function renderBreakdown() {
  const answers: Array<ReturnType<typeof deferred>> = []
  function Breakdown() {
    const [end, setEnd] = useState(1)
    const [level, setLevel] = useState('country')
    const query = useWindowQuery({
      queryKey: ['breakdown', { end_date: end, group_by: level }],
      queryFn: () => {
        const answer = deferred()
        answers.push(answer)
        return answer.promise
      },
    })
    return (
      <>
        <button type="button" onClick={() => setEnd((n) => n + 1)}>
          Move window
        </button>
        <button type="button" onClick={() => setLevel('region')}>
          Drill down
        </button>
        <p>{query.isPending ? 'loading' : query.data}</p>
      </>
    )
  }
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
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
