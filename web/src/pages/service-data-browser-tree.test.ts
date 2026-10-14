// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  containerPathLevel,
  findTreeNode,
  isExpectedNavigationRejection,
  resolveNodeListing,
  type PathNode,
} from './service-data-browser-tree'

// PostgreSQL: databases hold schemas, schemas hold tables.
const POSTGRES_LEVELS = {
  1: { can_list_containers: true, can_list_entities: false },
  2: { can_list_containers: false, can_list_entities: true },
} as const

describe('resolveNodeListing', () => {
  it('uses the capabilities the backend recorded on the node', () => {
    const schema = { canContainContainers: false, canContainEntities: true }

    expect(resolveNodeListing(schema, POSTGRES_LEVELS[1])).toEqual({
      listContainers: false,
      listEntities: true,
    })
  })

  it('does not request child containers for a schema missing from a stale snapshot', () => {
    // The node was not found (state captured before its parent loaded), so
    // the path's hierarchy level decides: level 2 = schema = tables only.
    const level = containerPathLevel('appdb/pg_catalog')

    expect(level).toBe(2)
    expect(resolveNodeListing(null, POSTGRES_LEVELS[2])).toEqual({
      listContainers: false,
      listEntities: true,
    })
  })

  it('still lists schemas for a database node', () => {
    expect(resolveNodeListing(null, POSTGRES_LEVELS[1])).toEqual({
      listContainers: true,
      listEntities: false,
    })
  })

  it('lets a partial node fall back per capability', () => {
    expect(
      resolveNodeListing({ canContainEntities: true }, POSTGRES_LEVELS[1])
    ).toEqual({ listContainers: true, listEntities: true })
  })
})

describe('findTreeNode', () => {
  const tree: PathNode[] = [
    {
      path: 'appdb',
      children: [
        { path: 'appdb/public', children: [] },
        { path: 'appdb/audit' },
      ],
    },
  ]

  it('finds nested nodes by full path', () => {
    expect(findTreeNode(tree, 'appdb/public')?.path).toBe('appdb/public')
  })

  it('returns null when the path is not in the tree', () => {
    expect(findTreeNode(tree, 'appdb/missing')).toBeNull()
  })
})

describe('isExpectedNavigationRejection', () => {
  it('treats a 400 problem as an expected answer', () => {
    expect(isExpectedNavigationRejection({ status: 400, detail: 'x' })).toBe(
      true
    )
  })

  it('does not hide server errors or non-objects', () => {
    expect(isExpectedNavigationRejection({ status: 500 })).toBe(false)
    expect(isExpectedNavigationRejection('boom')).toBe(false)
    expect(isExpectedNavigationRejection(null)).toBe(false)
  })
})
