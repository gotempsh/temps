// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

import { describeUpdatedPlacements } from './node-placements'

test('names each environment whose placement lost the removed node', () => {
  const text = describeUpdatedPlacements([
    {
      project_id: 1,
      project_name: 'shop',
      environment_id: 4,
      environment_name: 'production',
      target_nodes: [0, 1],
    },
    {
      project_id: 1,
      project_name: 'shop',
      environment_id: null,
      environment_name: null,
      target_nodes: [1],
    },
  ])
  expect(text).toContain('2 environments')
  expect(text).toContain('shop / production now targets control plane, node 1')
  expect(text).toContain('shop (project default) now targets node 1')
  expect(text).toContain('rejoins gets a new node ID')
})

test('says nothing when no placement named the node', () => {
  expect(describeUpdatedPlacements([])).toBeNull()
  expect(describeUpdatedPlacements(undefined)).toBeNull()
})
