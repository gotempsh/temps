// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  buildNodeDefaults,
  buildNodeFormSchema,
  buildNodeRequest,
  builderName,
  moveBuilder,
} from './build-node-form'

test('inherited effective nodes do not become a project override', () => {
  const values = buildNodeDefaults({
    source: 'global',
    project_id: 2,
    node_ids: null,
    effective_node_ids: [4, 7],
  })
  expect(values).toEqual({ mode: 'default', ids: [] })
  expect(buildNodeRequest(values)).toEqual({ node_ids: null })
})

test('explicit priority and a single worker pin round-trip unchanged', () => {
  for (const ids of [[7], [7, 4]]) {
    const values = buildNodeDefaults({
      source: 'project',
      node_ids: ids,
      effective_node_ids: ids,
    })
    expect(buildNodeRequest(buildNodeFormSchema.parse(values))).toEqual({
      node_ids: ids,
    })
  }
})

test('restoring default sends null even if the draft still holds workers', () => {
  expect(buildNodeRequest({ mode: 'default', ids: [7, 4] })).toEqual({
    node_ids: null,
  })
})

test('rejects empty custom pools, duplicates, virtual control plane, invalid and excessive IDs', () => {
  for (const ids of [
    [],
    [1, 1],
    [0],
    [-1],
    [1.5],
    [2147483648],
    Array.from({ length: 101 }, (_, i) => i + 1),
  ]) {
    expect(buildNodeFormSchema.safeParse({ mode: 'custom', ids }).success).toBe(
      false
    )
  }
  expect(
    buildNodeFormSchema.safeParse({ mode: 'default', ids: [] }).success
  ).toBe(true)
  expect(
    buildNodeFormSchema.safeParse({
      mode: 'custom',
      ids: Array.from({ length: 100 }, (_, i) => i + 1),
    }).success
  ).toBe(true)
})

test('reordering never drops workers or mutates cached selections', () => {
  const ids = [4, 7, 9]
  expect(moveBuilder(ids, 1, -1)).toEqual([7, 4, 9])
  expect(moveBuilder(ids, 1, 1)).toEqual([4, 9, 7])
  expect(moveBuilder(ids, 0, -1)).toEqual(ids)
  expect(moveBuilder(ids, 2, 1)).toEqual(ids)
  expect(ids).toEqual([4, 7, 9])
})

test('missing worker names retain their exact ID', () => {
  expect(builderName(7, [])).toBe('Worker #7')
})
