// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import { getErrorAttachmentUsageQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type { AttachmentUsage } from '@/api/client/types.gen'
import {
  attachmentContentUrl,
  bytesToMib,
  isWholeMib,
  isInlineImage,
  MAX_QUOTA_MIB,
  mibToBytes,
  pollWhileEmpty,
  EMPTY_LIST_POLL_MS,
  EMPTY_LIST_POLLS,
  quotaFormSchema,
  quotaShare,
  storeSavedUsage,
} from './attachments'

describe('attachments', () => {
  test('only raster images are shown inline, like the server serves them', () => {
    for (const inline of [
      'image/png',
      'image/jpeg',
      'image/gif',
      'image/webp',
    ]) {
      expect(isInlineImage(inline)).toBe(true)
    }
    for (const download of ['image/svg+xml', 'text/html', 'text/plain']) {
      expect(isInlineImage(download)).toBe(false)
    }
  })

  test('links the content endpoint of the project under the API base', () => {
    expect(attachmentContentUrl(3, 41, '/api')).toBe(
      '/api/projects/3/error-attachments/41/content'
    )
    // A console mounted with its own API base links there too.
    expect(
      attachmentContentUrl(3, 41, 'https://console.example.test/temps/api/')
    ).toBe(
      'https://console.example.test/temps/api/projects/3/error-attachments/41/content'
    )
  })

  test('an empty list is read again for a while, unless storage is missing', () => {
    const state = (files: number, reads: number) => ({
      state: {
        data: { data: Array(files).fill(null) },
        dataUpdateCount: reads,
      },
    })
    expect(pollWhileEmpty(true)(state(0, 1))).toBe(EMPTY_LIST_POLL_MS)
    expect(pollWhileEmpty(undefined)(state(0, 1))).toBe(EMPTY_LIST_POLL_MS)
    expect(pollWhileEmpty(true)(state(0, EMPTY_LIST_POLLS))).toBe(false)
    expect(pollWhileEmpty(true)(state(1, 1))).toBe(false)
    expect(pollWhileEmpty(false)(state(0, 1))).toBe(false)
  })

  test('the quota share stays between 0 and 1', () => {
    expect(quotaShare(0, 100)).toBe(0)
    expect(quotaShare(25, 100)).toBe(0.25)
    expect(quotaShare(150, 100)).toBe(1)
    expect(quotaShare(0, 0)).toBe(0)
    expect(quotaShare(1, 0)).toBe(1)
  })

  test('converts between bytes and MiB', () => {
    expect(bytesToMib(268_435_456)).toBe(256)
    expect(isWholeMib(268_435_456)).toBe(true)
    expect(isWholeMib(1)).toBe(false)
    expect(isWholeMib(0)).toBe(true)
    expect(mibToBytes(256)).toBe(268_435_456)
  })

  test('the quota form accepts 0 to 100 GiB in whole MiB', () => {
    expect(quotaFormSchema.safeParse({ quota_mib: 0 }).success).toBe(true)
    expect(
      quotaFormSchema.safeParse({ quota_mib: MAX_QUOTA_MIB }).success
    ).toBe(true)
    const over = quotaFormSchema.safeParse({ quota_mib: MAX_QUOTA_MIB + 1 })
    expect(over.error?.issues[0]?.message).toBe(
      'The quota can be at most 102,400 MiB (100 GiB).'
    )
    expect(
      quotaFormSchema.safeParse({ quota_mib: -1 }).error?.issues[0]?.message
    ).toBe('The quota cannot be negative.')
    expect(
      quotaFormSchema.safeParse({ quota_mib: 1.5 }).error?.issues[0]?.message
    ).toBe('Enter a whole number of MiB.')
    expect(
      quotaFormSchema.safeParse({ quota_mib: Number.NaN }).error?.issues[0]
        ?.message
    ).toBe('Enter the quota in MiB.')
  })
})

describe('storeSavedUsage', () => {
  const usage = (quota_bytes: number): AttachmentUsage => ({
    configured: true,
    reason: null,
    setup_path: null,
    used_bytes: 0,
    files: 0,
    quota_bytes,
    max_attachment_bytes: 2_097_152,
    max_event_attachment_bytes: 4_194_304,
    max_attachments_per_event: 8,
    dropped_over_quota: 0,
    dropped_queue_full: 0,
  })

  test('a read that started before the save cannot bring the old quota back', async () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    const queryKey = getErrorAttachmentUsageQueryKey({
      path: { project_id: 3 },
    })
    let answerOldRead: (value: AttachmentUsage) => void = () => {}
    const oldRead = queryClient
      .fetchQuery({
        queryKey,
        queryFn: () =>
          new Promise<AttachmentUsage>((resolve) => {
            answerOldRead = resolve
          }),
      })
      .catch(() => undefined)

    await storeSavedUsage(queryClient, 3, usage(1024))
    answerOldRead(usage(268_435_456))
    await oldRead

    expect(
      queryClient.getQueryData<AttachmentUsage>(queryKey)?.quota_bytes
    ).toBe(1024)
  })
})
