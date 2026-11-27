// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import {
  getProjectBySlugOptions,
  getProjectGroupBySlugQueryKey,
  getProjectGroupQueryKey,
  getProjectsInfiniteOptions,
  getProjectsOptions,
  listProjectGroupsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import {
  invalidateListsAfterServiceDelete,
  invalidateProjectLists,
} from './project-list-cache'

const generatedKey = getProjectsInfiniteOptions({
  query: { per_page: 100 },
}).queryKey
// Mirrors ProjectsProvider's user-scoped key.
const userScopedKey = ['getProjects', { page: 1, per_page: 4 }, 42] as const

function seededClient() {
  const client = new QueryClient()
  client.setQueryData(generatedKey, { pages: [], pageParams: [] })
  client.setQueryData(userScopedKey, { projects: [] })
  client.setQueryData(['getProjectBySlug', 'other'], { id: 1 })
  return client
}

test('invalidates generated-key project lists', async () => {
  const client = seededClient()
  await invalidateProjectLists(client)
  expect(client.getQueryState(generatedKey)?.isInvalidated).toBe(true)
})

test('invalidates the user-scoped project list the provider holds', async () => {
  const client = seededClient()
  await invalidateProjectLists(client)
  expect(client.getQueryState(userScopedKey)?.isInvalidated).toBe(true)
})

test('leaves unrelated queries alone', async () => {
  const client = seededClient()
  await invalidateProjectLists(client)
  expect(
    client.getQueryState(['getProjectBySlug', 'other'])?.isInvalidated
  ).toBe(false)
})

test('neither prefix alone reaches both key shapes', async () => {
  const onlyGenerated = seededClient()
  await onlyGenerated.invalidateQueries({ queryKey: ['getProjects'] })
  expect(onlyGenerated.getQueryState(generatedKey)?.isInvalidated).toBe(false)
})

describe('invalidateListsAfterServiceDelete', () => {
  // The sidebar's two lists: one page of services and every project (group)
  // with its members, under the key `useProjectGroups` reads.
  const treeServices = getProjectsOptions({
    query: { per_page: 100, page: 1 },
  }).queryKey
  const allGroups = [
    {
      ...listProjectGroupsQueryKey({ query: { page_size: 100 } })[0],
      allPages: true,
    },
  ]
  const groupPage = getProjectGroupQueryKey({ path: { group_id: 3 } })
  const groupBySlug = getProjectGroupBySlugQueryKey({ path: { slug: 'shop' } })
  const deletedService = getProjectBySlugOptions({
    path: { slug: 'gone' },
  }).queryKey

  function deleteClient() {
    const client = new QueryClient()
    for (const key of [
      treeServices,
      allGroups,
      groupPage,
      groupBySlug,
      deletedService,
      userScopedKey,
    ])
      client.setQueryData(key, {})
    return client
  }

  test('refreshes the lists the sidebar and the Projects page show', async () => {
    const client = deleteClient()
    await invalidateListsAfterServiceDelete(client)
    expect(client.getQueryState(treeServices)?.isInvalidated).toBe(true)
    expect(client.getQueryState(allGroups)?.isInvalidated).toBe(true)
    expect(client.getQueryState(userScopedKey)?.isInvalidated).toBe(true)
  })

  test('refreshes the projects that listed the service as a member', async () => {
    const client = deleteClient()
    await invalidateListsAfterServiceDelete(client)
    expect(client.getQueryState(groupPage)?.isInvalidated).toBe(true)
    expect(client.getQueryState(groupBySlug)?.isInvalidated).toBe(true)
  })

  test("leaves the deleted service's own record alone", async () => {
    // Its page is still mounted: a refetch would only answer 404.
    const client = deleteClient()
    await invalidateListsAfterServiceDelete(client)
    expect(client.getQueryState(deletedService)?.isInvalidated).toBe(false)
  })
})
