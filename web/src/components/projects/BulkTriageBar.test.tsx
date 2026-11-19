// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { BulkTriageBar, type BulkTriageAction } from './BulkTriageBar'

installBrowserWindow()
afterEach(cleanup)

test('the actions stay disabled until something is selected', () => {
  const actions: BulkTriageAction[] = []
  render(
    <BulkTriageBar
      pageGroupIds={[1, 2, 3]}
      selectedGroupIds={[]}
      onSelectionChange={() => {}}
      onAction={(action) => actions.push(action)}
      onMerge={() => {}}
      isPending={false}
    />
  )

  const resolve = screen.getByRole('button', { name: /resolve/i })
  expect(resolve.hasAttribute('disabled')).toBe(true)
  fireEvent.click(resolve)
  expect(actions).toEqual([])
})

test('a selection enables each action and reports which one was chosen', async () => {
  const actions: BulkTriageAction[] = []
  const ignores: unknown[] = []
  render(
    <BulkTriageBar
      pageGroupIds={[1, 2, 3]}
      selectedGroupIds={[1, 2]}
      onSelectionChange={() => {}}
      onAction={(action, ignore) => {
        actions.push(action)
        ignores.push(ignore)
      }}
      onMerge={() => {}}
      isPending={false}
    />
  )

  expect(screen.getByText('2 selected')).toBeTruthy()
  fireEvent.click(screen.getByRole('button', { name: /resolve/i }))
  // Ignore opens a menu of conditions for coming back.
  await act(async () => {
    fireEvent.pointerDown(screen.getByRole('button', { name: /ignore/i }), {
      button: 0,
      ctrlKey: false,
    })
  })
  await act(async () => {
    fireEvent.click(await screen.findByText('Until 100 more occurrences'))
  })
  fireEvent.click(screen.getByRole('button', { name: /reopen/i }))
  expect(actions).toEqual(['resolve', 'ignore', 'unresolve'])
  expect(ignores).toEqual([undefined, { event_count: 100 }, undefined])
})

test('select-all picks up this page only, and unchecking clears the selection', () => {
  const selections: number[][] = []
  const { rerender } = render(
    <BulkTriageBar
      pageGroupIds={[4, 5]}
      selectedGroupIds={[]}
      onSelectionChange={(ids) => selections.push(ids)}
      onAction={() => {}}
      onMerge={() => {}}
      isPending={false}
    />
  )

  fireEvent.click(
    screen.getByRole('checkbox', { name: /select every error group/i })
  )
  expect(selections).toEqual([[4, 5]])

  rerender(
    <BulkTriageBar
      pageGroupIds={[4, 5]}
      selectedGroupIds={[4, 5]}
      onSelectionChange={(ids) => selections.push(ids)}
      onAction={() => {}}
      onMerge={() => {}}
      isPending={false}
    />
  )
  fireEvent.click(
    screen.getByRole('checkbox', { name: /select every error group/i })
  )
  expect(selections[1]).toEqual([])
})

test('a request in flight disables every action', () => {
  render(
    <BulkTriageBar
      pageGroupIds={[1]}
      selectedGroupIds={[1]}
      onSelectionChange={() => {}}
      onAction={() => {}}
      onMerge={() => {}}
      isPending
    />
  )

  for (const name of [/resolve/i, /ignore/i, /reopen/i, /merge/i, /clear/i]) {
    expect(screen.getByRole('button', { name }).hasAttribute('disabled')).toBe(
      true
    )
  }
})

test('merge needs at least two groups and reports the request', () => {
  let merges = 0
  const { rerender } = render(
    <BulkTriageBar
      pageGroupIds={[7, 8]}
      selectedGroupIds={[7]}
      onSelectionChange={() => {}}
      onAction={() => {}}
      onMerge={() => {
        merges += 1
      }}
      isPending={false}
    />
  )

  const merge = () => screen.getByRole('button', { name: /merge/i })
  expect(merge().hasAttribute('disabled')).toBe(true)

  rerender(
    <BulkTriageBar
      pageGroupIds={[7, 8]}
      selectedGroupIds={[7, 8]}
      onSelectionChange={() => {}}
      onAction={() => {}}
      onMerge={() => {
        merges += 1
      }}
      isPending={false}
    />
  )
  expect(merge().hasAttribute('disabled')).toBe(false)
  fireEvent.click(merge())
  expect(merges).toBe(1)
})
