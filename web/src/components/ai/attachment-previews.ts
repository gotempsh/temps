// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ImageSupport } from './assistant-models'
import type { ChatAttachment } from './chat-message-parts'

export interface AttachmentLimits {
  maxFiles: number
  /** Largest file of any kind. */
  maxBytes: number
  /** Images are held to less where they are re-sent to the model. */
  maxImageBytes?: number
}

/** Workspace chats keep files on disk. */
export const WORKSPACE_ATTACHMENT_LIMITS: AttachmentLimits = {
  maxFiles: 8,
  maxBytes: 20 * 1024 * 1024,
}

/**
 * Assistant chats keep files in the database and re-send images to the model on
 * every step of a turn, so they are held to less. Mirrors the server's caps;
 * text files are held to 128 KB there, which only the server can judge by
 * content.
 */
export const ASSISTANT_ATTACHMENT_LIMITS: AttachmentLimits = {
  maxFiles: 4,
  maxBytes: 10 * 1024 * 1024,
  maxImageBytes: 5 * 1024 * 1024,
}

/** A file uploaded before its chat's panel mounted, to go with the first message. */
export interface OpeningAttachment {
  payload: Omit<ChatAttachment, 'preview_url'>
  file: File
}

function megabytes(bytes: number): string {
  return `${Math.round(bytes / (1024 * 1024))} MB`
}

/** Reject the entire selection rather than silently dropping excess files. */
export function attachmentSelectionError(
  files: readonly (Pick<File, 'name' | 'size'> & { type?: string })[],
  pendingCount: number,
  limits: AttachmentLimits = WORKSPACE_ATTACHMENT_LIMITS
): string | null {
  if (files.length + pendingCount > limits.maxFiles)
    return `A message may include at most ${limits.maxFiles} files.`
  const oversized = files.find((file) => file.size > limits.maxBytes)
  if (oversized)
    return `${oversized.name} exceeds the ${megabytes(limits.maxBytes)} attachment limit.`
  const { maxImageBytes } = limits
  const bigImage =
    maxImageBytes === undefined
      ? undefined
      : files.find(
          (file) => file.type?.startsWith('image/') && file.size > maxImageBytes
        )
  return bigImage && maxImageBytes !== undefined
    ? `${bigImage.name} exceeds the ${megabytes(maxImageBytes)} limit for images.`
    : null
}

/**
 * The assistant reads images only on models that accept them. Say so before
 * uploading, naming what to do, instead of letting the send fail later.
 */
export function assistantImageError(
  files: readonly Pick<File, 'name' | 'type'>[],
  support: ImageSupport
): string | null {
  if (support !== 'no') return null
  const image = files.find((file) => file.type.startsWith('image/'))
  return image
    ? `${image.name} is an image, and this model can only read text. Pick a model that accepts images, or attach a text file.`
    : null
}

/** Helper line under the composer. Names what this model can be given. */
export function attachHint(
  isAssistantChat: boolean,
  support: ImageSupport,
  limits: AttachmentLimits
): string {
  const size = megabytes(limits.maxBytes)
  if (!isAssistantChat) {
    return `Attach, drop, or paste images and files · up to ${limits.maxFiles} files, ${size} each`
  }
  const what =
    support === 'no'
      ? 'PDFs and files (this model can\u2019t read images)'
      : 'images, PDFs, and files'
  return `Attach, drop, or paste ${what} · up to ${limits.maxFiles} files, ${size} each`
}

/** Tooltip for the attach button in an assistant chat. */
export function assistantAttachTitle(
  support: ImageSupport,
  limits: AttachmentLimits,
  hasConversation: boolean
): string {
  if (!hasConversation) return 'Send a first message before attaching files'
  const size = megabytes(limits.maxBytes)
  const readable =
    'The assistant reads text files and the text of PDFs; other files it sees by name and size only.'
  if (support === 'no')
    return `Attach PDFs and files (${size} each). This model can\u2019t read images. ${readable}`
  if (support === 'unknown')
    return `Attach images, PDFs and files (${size} each). Images only work if the model can read them. ${readable}`
  return `Attach images, PDFs and files (${size} each). ${readable}`
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
