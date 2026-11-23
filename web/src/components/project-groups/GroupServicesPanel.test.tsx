// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import {
  GROUP_FANOUT_LIMIT,
  type GroupDatabase,
} from '@/lib/group-scoped-lists'
import { GroupDatabaseList } from './GroupServicesPanel'
import { summarize } from './useGroupResources'

installBrowserWindow()
afterEach(cleanup)

const cache = {
  id: 7,
  name: 'cache',
  type: 'redis',
  status: 'running',
  usedBy: [{ id: 11, name: 'web', slug: 'web' }],
} as unknown as GroupDatabase

function renderList(
  databases: GroupDatabase[],
  state: Partial<Parameters<typeof GroupDatabaseList>[0]['state']>
) {
  let retries = 0
  render(
    <MemoryRouter>
      <GroupDatabaseList
        databases={databases}
        state={{
          loading: false,
          failed: 0,
          truncated: false,
          refetch: () => {
            retries += 1
          },
          ...state,
        }}
      />
    </MemoryRouter>
  )
  return () => retries
}

describe("the project overview's databases", () => {
  test('a failed read offers a retry instead of an empty project', () => {
    const retries = renderList([], { failed: 1 })
    expect(
      screen.getByText('Linked databases could not be loaded for 1 service.')
    )
    expect(
      screen.queryByText('No databases are linked to these services yet.')
    ).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Retry databases' }))
    expect(retries()).toBe(1)
  })

  test('a partly failed read keeps what loaded and offers a retry', () => {
    const retries = renderList([cache], { failed: 2 })
    expect(screen.getByText('cache'))
    fireEvent.click(screen.getByRole('button', { name: 'Retry databases' }))
    expect(retries()).toBe(1)
  })

  test('a capped read says so, empty or not', () => {
    renderList([], { truncated: true })
    expect(
      screen.getByText(
        `None of the first ${GROUP_FANOUT_LIMIT} services has a linked database.`
      )
    )
    cleanup()
    renderList([cache], { truncated: true })
    expect(screen.getByText(/Only the first .* services are checked/))
  })

  test('rows that loaded stay visible while a retry loads', () => {
    renderList([cache], { loading: true })
    expect(screen.getByText('cache'))
  })

  test('a complete read offers no retry', () => {
    renderList([cache], {})
    expect(screen.queryByRole('button', { name: 'Retry databases' })).toBeNull()
  })
})

describe('retrying a scoped list', () => {
  test('re-requests only the failed requests', () => {
    const calls: string[] = []
    const result = (name: string, isError: boolean) => ({
      isLoading: false,
      isError,
      refetch: () => calls.push(name),
    })
    const state = summarize(
      [result('web', false), result('api', true), result('jobs', true)],
      false
    )
    expect(state.failed).toBe(2)
    state.refetch()
    expect(calls).toEqual(['api', 'jobs'])
  })
})
