// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  DropOffPoint,
  PageFlowEntry,
  PageTransition,
} from '@/api/client/types.gen'
import { sortTableRows } from '@/lib/client-table-sort'
import { durationSortValue } from './PageFlow'
import {
  dropOffSortValue,
  entryPageSortValue,
  exitPageSortValue,
  transitionSortValue,
  type EntryPageSortKey,
} from './PageFlow-shared'

test('entry and exit durations sort missing and nonpositive values together', () => {
  const rows = [
    { id: 'missing', duration: null },
    { id: 'long', duration: 12 },
    { id: 'zero', duration: 0 },
    { id: 'short', duration: 3 },
    { id: 'negative', duration: -2 },
  ]

  expect(
    sortTableRows(rows, (row) => durationSortValue(row.duration), 'asc').map(
      (row) => row.id
    )
  ).toEqual(['short', 'long', 'missing', 'zero', 'negative'])
  expect(
    sortTableRows(rows, (row) => durationSortValue(row.duration), 'desc').map(
      (row) => row.id
    )
  ).toEqual(['long', 'short', 'missing', 'zero', 'negative'])
})

describe('journey column sorting', () => {
  const page = (
    path: string,
    entries: number,
    views: number,
    bounce: number
  ): PageFlowEntry => ({
    page_path: path,
    entry_count: entries,
    total_views: views,
    bounce_rate: bounce,
    bounce_count: 0,
    entry_rate: 0,
    exit_count: entries,
    exit_rate: bounce,
    avg_time_on_page: null,
  })
  // Same shape as a reported table: Views read 15, 12, 1, 1, 1, 12 in the
  // default (Entries, descending) order.
  const pages = [
    page('/docs', 15, 15, 0.1),
    page('/pricing', 12, 12, 0.5),
    page('/a', 1, 1, 0.9),
    page('/b', 1, 1, 0.2),
    page('/c', 1, 1, 0.3),
    page('/blog/10', 1, 12, 0.4),
  ]
  const order = (key: EntryPageSortKey, direction: 'asc' | 'desc') =>
    sortTableRows(pages, (row) => entryPageSortValue(row, key), direction).map(
      (row) => row.page_path
    )

  test('Views reorders the rows in both directions, keeping ties in place', () => {
    expect(order('views', 'desc')).toEqual([
      '/docs',
      '/pricing',
      '/blog/10',
      '/a',
      '/b',
      '/c',
    ])
    expect(order('views', 'asc')).toEqual([
      '/a',
      '/b',
      '/c',
      '/pricing',
      '/blog/10',
      '/docs',
    ])
  })

  test('Entries, Bounce Rate and Page each map to their own field', () => {
    expect(order('entries', 'desc').slice(0, 2)).toEqual(['/docs', '/pricing'])
    expect(order('bounce', 'desc')[0]).toBe('/a')
    expect(order('bounce', 'asc')[0]).toBe('/docs')
    // Natural ordering: "/blog/10" sorts by its text, not by digit characters.
    expect(order('page', 'asc')).toEqual([
      '/a',
      '/b',
      '/blog/10',
      '/c',
      '/docs',
      '/pricing',
    ])
  })

  test('exit, drop-off and transition columns sort by their own fields', () => {
    const exits = [
      { ...pages[0], exit_count: 2, exit_rate: 0.2 },
      { ...pages[1], exit_count: 9, exit_rate: 0.1 },
    ]
    expect(
      sortTableRows(
        exits,
        (row) => exitPageSortValue(row, 'exits'),
        'desc'
      ).map((row) => row.page_path)
    ).toEqual(['/pricing', '/docs'])
    expect(
      sortTableRows(exits, (row) => exitPageSortValue(row, 'rate'), 'desc').map(
        (row) => row.page_path
      )
    ).toEqual(['/docs', '/pricing'])

    const drops = [
      { page_path: '/x', exit_count: 1, total_views: 40, exit_rate: 0.5 },
      { page_path: '/y', exit_count: 7, total_views: 9, exit_rate: 0.2 },
    ] as DropOffPoint[]
    expect(
      sortTableRows(drops, (row) => dropOffSortValue(row, 'views'), 'desc').map(
        (row) => row.page_path
      )
    ).toEqual(['/x', '/y'])

    const transitions = [
      { from_page: '/a', to_page: '/z', transition_count: 3, percentage: 10 },
      { from_page: '/b', to_page: '/y', transition_count: 8, percentage: 5 },
    ] as PageTransition[]
    expect(
      sortTableRows(
        transitions,
        (row) => transitionSortValue(row, 'to'),
        'asc'
      ).map((row) => row.to_page)
    ).toEqual(['/y', '/z'])
    expect(
      sortTableRows(
        transitions,
        (row) => transitionSortValue(row, 'count'),
        'desc'
      ).map((row) => row.from_page)
    ).toEqual(['/b', '/a'])
  })
})
