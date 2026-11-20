// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  appendAttachmentChunk,
  cancelAttachmentUpload,
  completeAttachmentUpload,
  startAttachmentUpload,
  uploadUserConversationAttachment,
} from '@/api/client'
import type { ChatAttachmentResponse } from '@/api/client'
import {
  needsChunking,
  UploadRequestError,
  uploadInChunks,
  type ChunkTransport,
} from './chunked-upload'

interface Problem {
  detail?: string
  title?: string
}

/** What a failed request says, preferring the server's own explanation. */
function failure(
  problem: unknown,
  response: Response | undefined,
  what: string
): UploadRequestError {
  const detail = (problem as Problem | undefined)?.detail
  const status = response?.status
  return new UploadRequestError(
    detail ?? `${what} failed (HTTP ${status ?? 'no response'}).`,
    status
  )
}

function transportFor(
  publicId: string
): ChunkTransport<ChatAttachmentResponse> {
  return {
    async start(file) {
      const { data, error, response } = await startAttachmentUpload({
        path: { public_id: publicId },
        body: {
          name: file.name,
          size_bytes: file.size,
          mime_type: file.type || null,
        },
      })
      if (!data)
        throw failure(error, response, `Starting the upload of ${file.name}`)
      return data
    },
    async append(uploadId, offset, chunk) {
      const { data, error, response } = await appendAttachmentChunk({
        path: { public_id: publicId, upload_id: uploadId },
        query: { offset },
        body: { chunk },
      })
      if (!data) throw failure(error, response, 'Sending a chunk')
      return data.received_bytes
    },
    async complete(uploadId) {
      const { data, error, response } = await completeAttachmentUpload({
        path: { public_id: publicId, upload_id: uploadId },
      })
      if (!data) throw failure(error, response, 'Finishing the upload')
      return data
    },
    async cancel(uploadId) {
      await cancelAttachmentUpload({
        path: { public_id: publicId, upload_id: uploadId },
      })
    },
  }
}

/**
 * Upload one file to a chat. A small file goes in a single request; anything
 * bigger goes up in chunks, so it also works behind a proxy that caps how large
 * a request body may be.
 */
export async function uploadChatAttachment(
  publicId: string,
  file: File,
  onProgress?: (receivedBytes: number, totalBytes: number) => void
): Promise<ChatAttachmentResponse> {
  if (!needsChunking(file.size)) {
    const { data, error, response } = await uploadUserConversationAttachment({
      path: { public_id: publicId },
      body: { file },
    })
    if (!data) throw failure(error, response, `Uploading ${file.name}`)
    return data
  }
  return uploadInChunks(file, transportFor(publicId), { onProgress })
}
