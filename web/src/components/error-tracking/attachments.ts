// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

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

/** The download URL of an attachment, answered with the session cookie. */
export function attachmentContentUrl(
  projectId: number,
  attachmentId: number
): string {
  return `/api/projects/${projectId}/error-attachments/${attachmentId}/content`
}

/** How full the quota is, 0 to 1. A zero quota with files is full. */
export function quotaShare(usedBytes: number, quotaBytes: number): number {
  if (quotaBytes <= 0) return usedBytes > 0 ? 1 : 0
  return Math.min(1, Math.max(0, usedBytes / quotaBytes))
}

export function bytesToMib(bytes: number): number {
  return Math.round(bytes / MIB)
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
