// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { StatusBucketBar } from './StatusBucketBar'

/** The tones a bar draws, bottom part first, with each part's weight. */
function drawn(markup: string): [string, number][] {
  return [...markup.matchAll(/data-tone="(\w+)"[^>]*flex-grow:\s*(\d+)/g)].map(
    (match) => [match[1], Number(match[2])]
  )
}

test('an hour that recovered shows its successful checks, not only its worst', () => {
  // 5 successful and 36 degraded checks: the bar used to be all yellow.
  const markup = renderToStaticMarkup(
    <StatusBucketBar
      bucket={{ operational_count: 5, degraded_count: 36, down_count: 0 }}
    />
  )
  expect(drawn(markup)).toEqual([
    ['operational', 5],
    ['degraded', 36],
  ])
  expect(markup).toContain('bg-green-500')
  expect(markup).toContain('bg-yellow-500')
  expect(markup).not.toContain('bg-red-500')
})

test('a single successful check among many still gets a visible sliver', () => {
  const markup = renderToStaticMarkup(
    <StatusBucketBar
      bucket={{ operational_count: 1, degraded_count: 0, down_count: 199 }}
    />
  )
  expect(drawn(markup)).toEqual([
    ['operational', 1],
    ['down', 199],
  ])
  expect(markup).toMatch(/data-tone="operational"[^>]*min-height:\s*3px/)
})

test('a bucket nothing checked is one grey block, never green', () => {
  const markup = renderToStaticMarkup(
    <StatusBucketBar
      bucket={{ operational_count: 0, degraded_count: 0, down_count: 0 }}
    />
  )
  expect(drawn(markup)).toEqual([])
  expect(markup).toContain('bg-gray-300')
  expect(markup).not.toContain('bg-green-500')
})
