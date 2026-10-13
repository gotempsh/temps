// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import {
  ACTIVE_BACKUP_POLL_INTERVAL_MS,
  externalServiceBackupsQueryKey,
  externalServiceBackupsRefetchInterval,
  listExternalServiceBackupsOptions,
  type ServiceBackupEntry,
  type ServiceBackupListResponse,
} from './external-service-backups'

function list(...states: string[]): ServiceBackupListResponse {
  return {
    backups: states.map(
      (state, index) =>
        ({
          id: index + 1,
          backup_id: `b-${index}`,
          state,
        }) as ServiceBackupEntry
    ),
    total: states.length,
    page: 1,
    page_size: 5,
  }
}

describe('service backups polling', () => {
  it('polls while a backup is queued or running', () => {
    expect(externalServiceBackupsRefetchInterval(list('pending'))).toBe(
      ACTIVE_BACKUP_POLL_INTERVAL_MS
    )
    expect(
      externalServiceBackupsRefetchInterval(list('completed', 'running'))
    ).toBe(ACTIVE_BACKUP_POLL_INTERVAL_MS)
  })

  it('stops once every backup is terminal or there are none', () => {
    expect(externalServiceBackupsRefetchInterval(list('completed'))).toBe(false)
    expect(externalServiceBackupsRefetchInterval(list('failed'))).toBe(false)
    expect(externalServiceBackupsRefetchInterval(list())).toBe(false)
    expect(externalServiceBackupsRefetchInterval(undefined)).toBe(false)
  })
})

describe('service backups invalidation', () => {
  it('the enqueue invalidation key matches every page of the card query', async () => {
    const client = new QueryClient()
    const pageOne = listExternalServiceBackupsOptions(7, 1, 5).queryKey
    const pageTwo = listExternalServiceBackupsOptions(7, 2, 5).queryKey
    const otherService = listExternalServiceBackupsOptions(8, 1, 5).queryKey
    for (const key of [pageOne, pageTwo, otherService]) {
      client.setQueryData(key, list())
    }

    await client.invalidateQueries({
      queryKey: externalServiceBackupsQueryKey(7),
    })

    const invalidated = (key: readonly unknown[]) =>
      client.getQueryState(key)?.isInvalidated
    expect(invalidated(pageOne)).toBe(true)
    expect(invalidated(pageTwo)).toBe(true)
    expect(invalidated(otherService)).toBe(false)
  })
})
