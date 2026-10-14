// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import {
  getProjectsInfiniteOptions,
  getProjectsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'

// The project list is cached under a generated `[{ _id: 'getProjects', ... }]`
// key. TemplateConfigurator invalidates it after creating a project from a
// template, so the helper it uses must reach paginated/infinite list entries.
function seededClient() {
  const client = new QueryClient()
  const key = getProjectsInfiniteOptions({ query: { per_page: 100 } }).queryKey
  client.setQueryData(key, { pages: [], pageParams: [] })
  return { client, key }
}

test('getProjectsQueryKey() invalidates a cached project list', async () => {
  const { client, key } = seededClient()
  await client.invalidateQueries({ queryKey: getProjectsQueryKey() })
  expect(client.getQueryState(key)?.isInvalidated).toBe(true)
})

test('a string-array key does not reach generated project list keys', async () => {
  const { client, key } = seededClient()
  await client.invalidateQueries({ queryKey: ['getProjects'] })
  expect(client.getQueryState(key)?.isInvalidated).toBe(false)
})
