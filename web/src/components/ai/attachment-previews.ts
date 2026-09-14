// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ChatAttachment } from './chat-message-parts'

/** Reject the entire selection rather than silently dropping excess files. */
export function attachmentSelectionError(
  files: readonly Pick<File, 'name' | 'size'>[],
  pendingCount: number
): string | null {
  if (files.length + pendingCount > 8)
    return 'A message may include at most 8 files.'
  const oversized = files.find((file) => file.size > 20 * 1024 * 1024)
  return oversized
    ? `${oversized.name} exceeds the 20 MB attachment limit.`
    : null
}

interface ObjectUrlApi {
  createObjectURL(blob: Blob): string
  revokeObjectURL(url: string): void
}

export function revokeAttachmentPreviews(
  attachments: Pick<ChatAttachment, 'preview_url'>[],
  objectUrls: Pick<ObjectUrlApi, 'revokeObjectURL'> = URL
): void {
  for (const attachment of attachments) {
    if (attachment.preview_url) {
      objectUrls.revokeObjectURL(attachment.preview_url)
    }
  }
}

/**
 * Materialize an uploaded attachment only while its owning composer exists.
 * The second check closes the narrow unmount race after creating a blob URL.
 */
export function createPendingAttachment(
  payload: Omit<ChatAttachment, 'preview_url'>,
  file: File,
  isMounted: () => boolean,
  objectUrls: ObjectUrlApi = URL
): ChatAttachment | null {
  if (!isMounted()) return null
  const previewUrl = payload.is_image
    ? objectUrls.createObjectURL(file)
    : undefined
  if (!isMounted()) {
    if (previewUrl) objectUrls.revokeObjectURL(previewUrl)
    return null
  }
  return { ...payload, preview_url: previewUrl }
}
