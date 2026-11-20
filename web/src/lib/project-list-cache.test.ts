// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import { getProjectsInfiniteOptions } from '@/api/client/@tanstack/react-query.gen'
import { invalidateProjectLists } from './project-list-cache'

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
