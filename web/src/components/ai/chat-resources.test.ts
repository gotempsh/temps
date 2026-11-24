// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { listChatResourcesQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type { ChatResource } from '@/api/client/types.gen'
import { QueryClient } from '@tanstack/react-query'
import {
  groupResources,
  invalidateChatResources,
  projectRefOf,
  resourceHref,
  resourceKeys,
  resourcesRefreshInterval,
} from './chat-resources'

function piece(overrides: Partial<ChatResource>): ChatResource {
  return {
    kind: 'project',
    id: '1',
    name: 'demo',
    created_at: '2026-10-17T17:41:04.462Z',
    ...overrides,
  }
}

describe('groupResources', () => {
  test('lists kinds in a fixed order and leaves out the empty ones', () => {
    const groups = groupResources([
      piece({ kind: 'sandbox', id: 'sbx_1' }),
      piece({ kind: 'service', id: '2' }),
      piece({ kind: 'project', id: '3' }),
      piece({ kind: 'project', id: '4' }),
    ])
    expect(groups.map((group) => group.kind)).toEqual([
      'project',
      'service',
      'sandbox',
    ])
    expect(groups[0].resources.map((resource) => resource.id)).toEqual([
      '3',
      '4',
    ])
  })

  test('has no groups when nothing was made', () => {
    expect(groupResources([])).toEqual([])
  })
})

describe('resourceHref', () => {
  test('points each kind at the console page that shows it', () => {
    expect(resourceHref(piece({ kind: 'project', slug: 'demo-app' }))).toBe(
      '/projects/demo-app'
    )
    expect(resourceHref(piece({ kind: 'service', id: '7' }))).toBe('/storage/7')
    expect(resourceHref(piece({ kind: 'sandbox', id: 'sbx_9' }))).toBe(
      '/sandboxes/sbx_9'
    )
  })

  test('has no page for a repository or a project without a slug', () => {
    expect(resourceHref(piece({ kind: 'repository', id: 'a/b' }))).toBeNull()
    expect(resourceHref(piece({ kind: 'project', slug: null }))).toBeNull()
  })
})

describe('projectRefOf', () => {
  test('turns a project piece into a project reference', () => {
    expect(
      projectRefOf(piece({ id: '36', slug: 'nextjs-app', name: 'nextjs-app' }))
    ).toEqual({ id: 36, slug: 'nextjs-app', name: 'nextjs-app' })
  })

  test('is null for other kinds and for ids that are not numbers', () => {
    expect(projectRefOf(piece({ kind: 'service', slug: 'x' }))).toBeNull()
    expect(projectRefOf(piece({ id: 'abc', slug: 'x' }))).toBeNull()
    expect(projectRefOf(piece({ slug: null }))).toBeNull()
  })
})

describe('resourcesRefreshInterval', () => {
  test('keeps checking while a service is still starting', () => {
    expect(
      resourcesRefreshInterval([piece({ kind: 'service', status: 'starting' })])
    ).toBeGreaterThan(0)
  })

  test('stops once everything has settled', () => {
    expect(
      resourcesRefreshInterval([
        piece({ kind: 'service', status: 'running' }),
        piece({ kind: 'project', status: null }),
      ])
    ).toBe(false)
    expect(resourcesRefreshInterval([])).toBe(false)
  })
})

describe('resourceKeys', () => {
  test('tells two kinds apart even when their ids match', () => {
    expect(
      resourceKeys([
        piece({ kind: 'project', id: '2' }),
        piece({ kind: 'service', id: '2' }),
      ])
    ).toEqual(['project:2', 'service:2'])
  })
})

describe('invalidateChatResources', () => {
  test("marks every chat's pieces stale and leaves other queries alone", async () => {
    const client = new QueryClient()
    const first = listChatResourcesQueryKey({ path: { public_id: 'conv_a' } })
    const second = listChatResourcesQueryKey({ path: { public_id: 'conv_b' } })
    const other = ['something-else']
    client.setQueryData(first, { resources: [], truncated: false })
    client.setQueryData(second, { resources: [], truncated: false })
    client.setQueryData(other, 1)

    await invalidateChatResources(client)

    expect(client.getQueryState(first)?.isInvalidated).toBe(true)
    expect(client.getQueryState(second)?.isInvalidated).toBe(true)
    expect(client.getQueryState(other)?.isInvalidated).toBe(false)
  })
})
