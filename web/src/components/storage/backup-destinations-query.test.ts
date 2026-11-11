// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { backupDestinationsOptions } from './backup-destinations-query'

async function observeFailure(error: unknown) {
  const client = new QueryClient()
  const observer = new QueryObserver(client, {
    ...backupDestinationsOptions(),
    queryFn: () => Promise.reject(error),
    retry: false,
  })
  const stop = observer.subscribe(() => {})
  await observer.refetch().catch(() => {})
  const failures = collectRouteQueryFailures(client)
  stop()
  return failures
}

test('a refused destination list is not reported by the route banner', async () => {
  expect(
    await observeFailure({ status: 403, title: 'Forbidden', detail: 'no' })
  ).toHaveLength(0)
})

test('a failing destination list is still reported', async () => {
  expect(
    await observeFailure({ status: 500, title: 'Boom', detail: 'down' })
  ).toHaveLength(1)
})
