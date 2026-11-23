// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, render, screen } from '@testing-library/react'
import { Globe } from 'lucide-react'
import { installBrowserWindow } from '@/test/dom'
import { GROUP_FANOUT_LIMIT } from '@/lib/group-scoped-lists'
import { ScopedList } from './GroupScopedPages'

installBrowserWindow()
afterEach(cleanup)

const complete = { loading: false, failed: 0, truncated: false, refetch() {} }

function renderList(state: typeof complete, isEmpty: boolean) {
  render(
    <ScopedList
      state={state}
      isEmpty={isEmpty}
      noun="domains"
      icon={Globe}
      emptyTitle="No custom domains in this project yet"
      emptyDescription="Domains are added to a service."
      emptyAction={<button type="button">Add a domain</button>}
    >
      <p>the list</p>
    </ScopedList>
  )
}

describe('a scoped list', () => {
  test('is empty only when every request succeeded', () => {
    renderList(complete, true)
    expect(screen.getByText('No custom domains in this project yet'))
    expect(screen.getByRole('button', { name: 'Add a domain' }))
  })

  test('a failed read is a failure with a retry, not an empty project', () => {
    renderList({ ...complete, failed: 2 }, true)
    expect(
      screen.queryByText('No custom domains in this project yet')
    ).toBeNull()
    expect(screen.queryByRole('button', { name: 'Add a domain' })).toBeNull()
    expect(screen.getByText("This project's domains could not be read in full"))
    expect(screen.getByText("2 requests for this project's domains failed."))
    expect(screen.getByRole('button', { name: 'Retry domains' }))
  })

  test('an empty capped read says what was left out', () => {
    renderList({ ...complete, truncated: true }, true)
    expect(
      screen.queryByText('No custom domains in this project yet')
    ).toBeNull()
    expect(
      screen.getByText(
        `Only the first ${GROUP_FANOUT_LIMIT} services are included, and they have no domains.`
      )
    )
  })

  test('shows a skeleton only while there is nothing to show', () => {
    renderList({ ...complete, loading: true }, true)
    expect(screen.getByRole('status', { name: 'Loading domains' }))
    cleanup()
    renderList({ ...complete, loading: true }, false)
    expect(screen.getByText('the list'))
    expect(screen.queryByRole('status', { name: 'Loading domains' })).toBeNull()
  })

  test('a partial list shows what it has and says it is partial', () => {
    renderList({ ...complete, truncated: true }, false)
    expect(screen.getByText('the list'))
    expect(screen.getByText('This list may be incomplete'))
  })
})
