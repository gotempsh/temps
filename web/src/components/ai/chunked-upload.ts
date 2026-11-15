// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Uploading an attachment in small pieces.
 *
 * A reverse proxy in front of the console may cap a request body well below
 * the size of a photo (a Fleet preview refuses anything over 40 KiB), so a
 * single multipart request cannot carry it. The file goes up as a start
 * request, a run of chunks no bigger than the server allows, and a completion
 * that stores it. Files small enough for one request skip all of that.
 */

/**
 * The largest file sent as one request. The multipart framing and headers add
 * a few hundred bytes, so this stays clear of a 40 KiB proxy cap.
 */
export const SINGLE_REQUEST_MAX_BYTES = 24 * 1024

/** What the server tells a client when an upload opens. */
export interface UploadStarted {
  upload_id: string
  chunk_bytes: number
}

/** A failed request, with the HTTP status when there was a response. */
export class UploadRequestError extends Error {
  readonly status: number | undefined
  constructor(message: string, status?: number) {
    super(message)
    this.name = 'UploadRequestError'
    this.status = status
  }
}

/** The four calls an upload makes; the app supplies them from its API client. */
export interface ChunkTransport<T> {
  start(file: {
    name: string
    size: number
    type: string
  }): Promise<UploadStarted>
  append(uploadId: string, offset: number, chunk: Blob): Promise<number>
  complete(uploadId: string): Promise<T>
  cancel(uploadId: string): Promise<void>
}

export function needsChunking(size: number): boolean {
  return size > SINGLE_REQUEST_MAX_BYTES
}

/** The `[start, end)` byte ranges that cover `size` bytes in pieces of at most `chunkBytes`. */
export function chunkRanges(
  size: number,
  chunkBytes: number
): Array<{ start: number; end: number }> {
  if (!Number.isFinite(size) || size <= 0) return []
  if (!Number.isFinite(chunkBytes) || chunkBytes < 1) {
    throw new RangeError(
      `chunk size must be at least 1 byte, got ${chunkBytes}`
    )
  }
  const ranges: Array<{ start: number; end: number }> = []
  for (let start = 0; start < size; start += chunkBytes) {
    ranges.push({ start, end: Math.min(start + chunkBytes, size) })
  }
  return ranges
}

/** A failure worth sending the same chunk again for: no response, or the server erred. */
export function isRetryable(error: unknown): boolean {
  if (error instanceof UploadRequestError) {
    return error.status === undefined || error.status >= 500
  }
  return error instanceof TypeError
}

export interface UploadOptions {
  /** Called with the bytes the server holds, after each chunk. */
  onProgress?: (receivedBytes: number, totalBytes: number) => void
  /** Extra attempts for one chunk after a retryable failure. Default 2. */
  retries?: number
  /** Waits between attempts; replaced in tests. */
  pause?: (attempt: number) => Promise<void>
}

const defaultPause = (attempt: number) =>
  new Promise<void>((resolve) => setTimeout(resolve, 300 * attempt))

/**
 * Send `file` in chunks and return what the completion answers. A failure
 * cancels the upload (best effort) so the server frees what it reserved, then
 * rethrows the original error.
 */
export async function uploadInChunks<T>(
  file: File,
  transport: ChunkTransport<T>,
  options: UploadOptions = {}
): Promise<T> {
  const retries = options.retries ?? 2
  const pause = options.pause ?? defaultPause
  const started = await transport.start({
    name: file.name,
    size: file.size,
    type: file.type,
  })
  try {
    for (const { start, end } of chunkRanges(file.size, started.chunk_bytes)) {
      const piece = file.slice(start, end)
      let attempt = 0
      for (;;) {
        try {
          const received = await transport.append(
            started.upload_id,
            start,
            piece
          )
          options.onProgress?.(received, file.size)
          break
        } catch (error) {
          attempt += 1
          if (attempt > retries || !isRetryable(error)) throw error
          await pause(attempt)
        }
      }
    }
    return await transport.complete(started.upload_id)
  } catch (error) {
    await transport.cancel(started.upload_id).catch(() => undefined)
    throw error
  }
}
