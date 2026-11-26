// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps ai chat upload <conversation> <file>`: attach a file to a
 * conversation through the chunked upload API (start, append chunks in
 * order, complete), resumable after an interrupted run.
 *
 * The server holds an upload in memory for a short while and accepts chunks
 * only in order. A retried chunk it already holds is accepted again, and a
 * chunk at the wrong offset is refused with 409 and the offset it expects,
 * so `--resume <upload-id>` needs no local state: the first chunk tells the
 * CLI where the server stopped.
 */

import type { Command } from 'commander'
import { basename } from 'node:path'
import { client, getErrorMessage } from '../../lib/api-client.js'
import { appendAttachmentChunk, completeAttachmentUpload, startAttachmentUpload } from '../../api/sdk.gen.js'
import { connect, output, parseIntegerFlag } from '../../lib/command-kit.js'
import { subgroup } from '../../lib/endpoint.js'
import { ValidationError } from '../../utils/errors.js'
import { info } from '../../ui/output.js'

/** The server's chunk limit (`UPLOAD_CHUNK_BYTES`); the start reply names the real one. */
export const DEFAULT_CHUNK_BYTES = 32 * 1024
const MAX_ATTEMPTS = 3

export interface UploadOptions {
  mimeType?: string
  name?: string
  resume?: string
  chunkBytes?: string
  json?: boolean
}

/** The offset a 409 "Chunk Out Of Order" reply says the server expects, if it names one. */
export function expectedOffset(detail: string): number | undefined {
  const match = detail.match(/expected the next chunk at offset (\d+)/)
  return match ? Number(match[1]) : undefined
}

function detailOf(error: unknown): string {
  if (error && typeof error === 'object' && 'detail' in error) return String((error as { detail: unknown }).detail)
  return getErrorMessage(error)
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

export async function uploadAction(conversation: string, file: string, options: UploadOptions): Promise<void> {
  const source = Bun.file(file)
  if (!(await source.exists())) throw new ValidationError(`File not found: ${file}`)
  const size = source.size
  if (size === 0) throw new ValidationError(`${file} is empty; an attachment needs at least one byte`)
  await connect()

  let uploadId = options.resume
  let chunkBytes = options.chunkBytes ? parseIntegerFlag(options.chunkBytes, '--chunk-bytes', 1) : DEFAULT_CHUNK_BYTES
  if (!uploadId) {
    const { data, error } = await startAttachmentUpload({
      client,
      path: { public_id: conversation },
      body: { name: options.name ?? basename(file), size_bytes: size, mime_type: options.mimeType ?? (source.type || null) },
    })
    if (error || !data) throw new Error(`Could not start uploading ${file} to conversation ${conversation}: ${getErrorMessage(error)}`)
    uploadId = data.upload_id
    chunkBytes = Math.min(chunkBytes, data.chunk_bytes)
    // On stderr, so it is there to resume with even when stdout is --json.
    console.error(`Upload ${uploadId} started; if it is interrupted, resume with: temps ai chat upload ${conversation} ${file} --resume ${uploadId}`)
  }

  let offset = 0
  while (offset < size) {
    const end = Math.min(offset + chunkBytes, size)
    const chunk = source.slice(offset, end)
    let attempt = 0
    for (;;) {
      attempt++
      const { data, error, response } = await appendAttachmentChunk({
        client,
        path: { public_id: conversation, upload_id: uploadId },
        query: { offset },
        body: { chunk },
      })
      if (data && !error) {
        offset = data.received_bytes
        break
      }
      const status = response?.status ?? 0
      const expected = status === 409 ? expectedOffset(detailOf(error)) : undefined
      if (expected !== undefined) {
        // The server holds a different prefix (a resumed or retried upload): continue from there.
        if (expected === offset) throw new Error(`Upload ${uploadId} refused bytes ${offset}-${end} of ${file}: ${detailOf(error)}`)
        if (expected > size) throw new Error(`Upload ${uploadId} already holds ${expected} bytes, more than ${file} (${size} bytes): it is a different file`)
        if (options.resume && expected !== offset) info(`Resuming upload ${uploadId} at byte ${expected} of ${size}`)
        offset = expected
        break
      }
      // Retry what may be transient (no reply, or a 5xx); refuse everything else at once.
      if ((status === 0 || status >= 500) && attempt < MAX_ATTEMPTS) {
        await sleep(250 * 2 ** attempt)
        continue
      }
      throw new Error(
        `Could not upload bytes ${offset}-${end} of ${file} (upload ${uploadId}): ${detailOf(error)}` +
          (status === 404 ? ' The upload expired or was cancelled; start again without --resume.' : ` Resume with --resume ${uploadId}.`),
      )
    }
  }

  const { data, error } = await completeAttachmentUpload({ client, path: { public_id: conversation, upload_id: uploadId } })
  if (error || !data) throw new Error(`Could not complete upload ${uploadId} of ${file}: ${getErrorMessage(error)}`)
  output(data, options, `Attached ${basename(file)}`, `Uploaded ${size} bytes`)
}

export function registerAiUploadCommand(ai: Command): void {
  subgroup(ai, 'chat', 'AI chat conversations: messages, attachments, sandboxes and workspaces')
    .command('upload <conversation-id> <file>')
    .description('Attach a file to a conversation in chunks; resumable with --resume')
    .option('--name <name>', 'File name to show (default: the file name)')
    .option('--mime-type <type>', 'Content type (default: guessed from the extension)')
    .option('--resume <upload-id>', 'Continue an interrupted upload instead of starting a new one')
    .option('--chunk-bytes <n>', `Bytes per chunk (default ${DEFAULT_CHUNK_BYTES}, the server's limit)`)
    .option('--json', 'Output in JSON format')
    .action(uploadAction)
}
