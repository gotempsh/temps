// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type { QueryClient } from '@tanstack/react-query'
import { getErrorAttachmentUsageQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { client } from '@/api/client/client.gen'
import type { AttachmentUsage } from '@/api/client/types.gen'

/** The types the server serves inline; everything else is a download. */
const INLINE_IMAGE_TYPES = new Set([
  'image/png',
  'image/jpeg',
  'image/gif',
  'image/webp',
])

/** Shown where attachments would appear, so the feature explains itself. */
export const ATTACHMENT_EXAMPLE =
  "Sentry.getCurrentScope().addAttachment({ filename: 'app.log', data: logText })"

/** Where blob storage is enabled when the server does not say. */
export const STORAGE_SETUP_PATH = '/storage'

const MIB = 1024 * 1024
/** Largest quota the server accepts: 100 GiB. */
export const MAX_QUOTA_MIB = 100 * 1024

export function isInlineImage(contentType: string): boolean {
  return INLINE_IMAGE_TYPES.has(contentType)
}

/**
 * The download URL of an attachment, answered with the session cookie. It
 * starts at the API base the console was mounted with, like every SDK call.
 */
export function attachmentContentUrl(
  projectId: number,
  attachmentId: number,
  baseUrl: string = client.getConfig().baseUrl ?? '/api'
): string {
  const base = baseUrl.replace(/\/+$/, '')
  return `${base}/projects/${projectId}/error-attachments/${attachmentId}/content`
}

/** How often, and how many times, an empty list is read again. */
export const EMPTY_LIST_POLL_MS = 5_000
export const EMPTY_LIST_POLLS = 12

/**
 * The `refetchInterval` of an attachment list. Files are stored just after
 * their event or feedback, so a list read right after it arrived can be
 * empty: it is read again every few seconds, for about a minute, unless
 * blob storage is known to be missing.
 */
export function pollWhileEmpty(storageConfigured: boolean | undefined) {
  return (query: {
    state: { data?: { data: unknown[] }; dataUpdateCount: number }
  }): number | false =>
    storageConfigured !== false &&
    query.state.data?.data.length === 0 &&
    query.state.dataUpdateCount < EMPTY_LIST_POLLS
      ? EMPTY_LIST_POLL_MS
      : false
}

/** How full the quota is, 0 to 1. A zero quota with files is full. */
export function quotaShare(usedBytes: number, quotaBytes: number): number {
  if (quotaBytes <= 0) return usedBytes > 0 ? 1 : 0
  return Math.min(1, Math.max(0, usedBytes / quotaBytes))
}

export function bytesToMib(bytes: number): number {
  return Math.round(bytes / MIB)
}

/**
 * Put the usage a successful quota save answered with in the cache. A read
 * that started before the save committed would bring the old quota back
 * after it, so reads still in flight are cancelled first.
 */
export async function storeSavedUsage(
  queryClient: QueryClient,
  projectId: number,
  saved: AttachmentUsage
): Promise<void> {
  const queryKey = getErrorAttachmentUsageQueryKey({
    path: { project_id: projectId },
  })
  await queryClient.cancelQueries({ queryKey })
  queryClient.setQueryData(queryKey, saved)
}

/** Whether `bytes` shows exactly as a number of MiB in the quota form. */
export function isWholeMib(bytes: number): boolean {
  return bytes % MIB === 0
}

export function mibToBytes(mib: number): number {
  return mib * MIB
}

/** The attachment quota form, in MiB. */
export const quotaFormSchema = z.object({
  quota_mib: z
    .number({ error: 'Enter the quota in MiB.' })
    .int('Enter a whole number of MiB.')
    .min(0, 'The quota cannot be negative.')
    .max(
      MAX_QUOTA_MIB,
      `The quota can be at most ${MAX_QUOTA_MIB.toLocaleString('en-US')} MiB (100 GiB).`
    ),
})
export type QuotaFormData = z.infer<typeof quotaFormSchema>
