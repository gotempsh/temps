// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, spyOn, test } from 'bun:test'
import {
  QueryClient,
  QueryClientProvider,
  keepPreviousData,
  useQuery,
} from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { useState, type ReactNode } from 'react'
import { toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { ROUTE_FAILURE_IGNORE_REFUSAL } from '@/lib/route-query-failures'
import { RefreshButton } from './refresh-button'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

function renderWithClient(ui: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>)
}

function refreshButton() {
  return screen.getByRole('button', { name: 'Refresh' })
}

describe('RefreshButton', () => {
  test('refetches only the queries it is given', async () => {
    const calls = { rows: 0, other: 0 }
    function Page() {
      const rows = useQuery({
        queryKey: ['rows', { page: 2 }],
        queryFn: async () => `rows ${++calls.rows}`,
      })
      useQuery({ queryKey: ['other'], queryFn: async () => ++calls.other })
      return (
        <>
          <RefreshButton queryKeys={[['rows']]} />
          <p>{rows.data}</p>
        </>
      )
    }
    renderWithClient(<Page />)
    await screen.findByText('rows 1')

    fireEvent.click(refreshButton())

    await screen.findByText('rows 2')
    expect(calls.other).toBe(1)
  })

  test('is busy and spinning while fetching, keeps the rows, and ignores clicks', async () => {
    const answers = [deferred<string>(), deferred<string>()]
    let calls = 0
    function Page() {
      const rows = useQuery({
        queryKey: ['rows'],
        queryFn: () => answers[calls++].promise,
      })
      return (
        <>
          <RefreshButton queryKeys={[['rows']]} />
          <p>{rows.data ?? 'loading'}</p>
        </>
      )
    }
    renderWithClient(<Page />)
    await act(async () => answers[0].resolve('first rows'))
    await screen.findByText('first rows')
    expect(refreshButton().getAttribute('aria-busy')).toBe('false')

    fireEvent.click(refreshButton())
    await waitFor(() =>
      expect(refreshButton().getAttribute('aria-disabled')).toBe('true')
    )
    expect(refreshButton().getAttribute('aria-busy')).toBe('true')
    expect(
      refreshButton().querySelector('svg')?.getAttribute('class')
    ).toContain('animate-spin')
    // The current rows stay on screen while the refresh runs.
    expect(screen.getByText('first rows')).toBeTruthy()

    fireEvent.click(refreshButton())
    expect(calls).toBe(2)

    await act(async () => answers[1].resolve('second rows'))
    await screen.findByText('second rows')
    await waitFor(() =>
      expect(refreshButton().getAttribute('aria-disabled')).toBe('false')
    )
    expect(
      refreshButton().querySelector('svg')?.getAttribute('class')
    ).not.toContain('animate-spin')
  })

  test('says the data was updated after a successful refresh', async () => {
    function Page() {
      useQuery({ queryKey: ['rows'], queryFn: async () => 'rows' })
      return <RefreshButton queryKeys={[['rows']]} />
    }
    renderWithClient(<Page />)
    expect(screen.queryByText(/Updated/)).toBeNull()

    fireEvent.click(refreshButton())

    await screen.findByText('Updated just now')
  })

  test('shows the API error detail in a toast when the refresh fails', async () => {
    const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
    let calls = 0
    function Page() {
      const rows = useQuery({
        queryKey: ['rows'],
        queryFn: async () => {
          if (++calls > 1)
            throw { title: 'Forbidden', detail: 'Missing LogsRead' }
          return 'first rows'
        },
      })
      return (
        <>
          <RefreshButton queryKeys={[['rows']]} />
          <p>{rows.data}</p>
        </>
      )
    }
    renderWithClient(<Page />)
    await screen.findByText('first rows')

    fireEvent.click(refreshButton())

    await waitFor(() =>
      expect(failure).toHaveBeenCalledWith("Couldn't refresh", {
        description: 'Missing LogsRead',
      })
    )
    expect(screen.getByText('first rows')).toBeTruthy()
    expect(screen.queryByText(/Updated/)).toBeNull()
  })

  test('a refusal the page expects is not a failed refresh', async () => {
    const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
    function Page() {
      const rows = useQuery({ queryKey: ['rows'], queryFn: () => 'rows' })
      // Optional context this role may not read, as on the Domains page.
      useQuery({
        queryKey: ['tunnels'],
        queryFn: async () => {
          throw { status: 403, title: 'Forbidden', detail: 'Missing settings' }
        },
        meta: { routeFailure: ROUTE_FAILURE_IGNORE_REFUSAL },
      })
      return (
        <>
          <RefreshButton queryKeys={[['rows'], ['tunnels']]} />
          <p>{rows.data}</p>
        </>
      )
    }
    renderWithClient(<Page />)
    await screen.findByText('rows')

    fireEvent.click(refreshButton())

    await screen.findByText('Updated just now')
    expect(failure).not.toHaveBeenCalled()
  })

  test('any other failure of a refusal-tolerant query still fails the refresh', async () => {
    const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
    let calls = 0
    function Page() {
      const tunnels = useQuery({
        queryKey: ['tunnels'],
        queryFn: async () => {
          if (++calls > 1)
            throw { status: 500, title: 'Internal', detail: 'Store is down' }
          return 'tunnels'
        },
        meta: { routeFailure: ROUTE_FAILURE_IGNORE_REFUSAL },
      })
      return (
        <>
          <RefreshButton queryKeys={[['tunnels']]} />
          <p>{tunnels.data}</p>
        </>
      )
    }
    renderWithClient(<Page />)
    await screen.findByText('tunnels')

    fireEvent.click(refreshButton())

    await waitFor(() =>
      expect(failure).toHaveBeenCalledWith("Couldn't refresh", {
        description: 'Store is down',
      })
    )
    expect(screen.queryByText(/Updated/)).toBeNull()
  })

  test('moves a relative window first and waits for its query', async () => {
    const answers = new Map<number, ReturnType<typeof deferred<string>>>()
    function Page() {
      const [windowEnd, setWindowEnd] = useState(1)
      const rows = useQuery({
        queryKey: ['rows', { windowEnd }],
        queryFn: () => {
          const answer = deferred<string>()
          answers.set(windowEnd, answer)
          return answer.promise
        },
        placeholderData: keepPreviousData,
      })
      return (
        <>
          <RefreshButton
            queryKeys={[['rows']]}
            onRefresh={() => setWindowEnd((end) => end + 1)}
          />
          <p>{rows.data ?? 'loading'}</p>
        </>
      )
    }
    renderWithClient(<Page />)
    await waitFor(() => expect(answers.has(1)).toBe(true))
    await act(async () => answers.get(1)!.resolve('window 1'))
    await screen.findByText('window 1')

    fireEvent.click(refreshButton())

    await waitFor(() => expect(answers.has(2)).toBe(true))
    expect(refreshButton().getAttribute('aria-busy')).toBe('true')
    expect(screen.getByText('window 1')).toBeTruthy()
    expect(screen.queryByText(/Updated/)).toBeNull()

    await act(async () => answers.get(2)!.resolve('window 2'))
    await screen.findByText('window 2')
    await screen.findByText('Updated just now')
  })

  test('never reloads the document', async () => {
    const reload = spyOn(window.location, 'reload').mockImplementation(() => {})
    function Page() {
      useQuery({ queryKey: ['rows'], queryFn: async () => 'rows' })
      return <RefreshButton queryKeys={[['rows']]} />
    }
    renderWithClient(<Page />)

    fireEvent.click(refreshButton())
    await screen.findByText('Updated just now')

    expect(reload).not.toHaveBeenCalled()
    reload.mockRestore()
  })
})
