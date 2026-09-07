// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Unzip, UnzipInflate, type UnzipFile } from 'fflate'
import {
  MAX_LOCAL_IMPORT_BYTES,
  MAX_LOCAL_IMPORT_FILE_BYTES,
  MAX_LOCAL_IMPORT_FILES,
  isSensitiveLocalImportPath,
  normalizedWorkspaceImportPath,
} from './workspace-import-policy'

type ExtractRequest = { archive: File }
type ExtractResponse =
  | {
      type: 'entry'
      name: string
      path: string
      chunks: ArrayBuffer[]
    }
  | { type: 'done'; skipped: string[] }
  | { type: 'error'; message: string }

interface WorkspaceImportWorkerScope {
  onmessage: ((event: MessageEvent<ExtractRequest>) => void) | null
  postMessage(message: ExtractResponse, transfer?: Transferable[]): void
}

const workerScope = self as unknown as WorkspaceImportWorkerScope
const MAX_COMPRESSED_CHUNK_BYTES = 4 * 1024

function errorMessage(cause: unknown): string {
  return cause instanceof Error
    ? cause.message
    : 'The ZIP archive could not be extracted.'
}

async function extract(archive: File): Promise<void> {
  const skipped: string[] = []
  const activeFiles = new Set<UnzipFile>()
  let discoveredEntries = 0
  let extractedBytes = 0
  let failure: Error | null = null

  const fail = (cause: unknown) => {
    if (failure) return
    failure = cause instanceof Error ? cause : new Error(errorMessage(cause))
    for (const active of activeFiles) active.terminate()
  }

  const unzip = new Unzip((entry) => {
    if (failure) return
    discoveredEntries += 1
    if (discoveredEntries > MAX_LOCAL_IMPORT_FILES) {
      fail(
        new Error(
          `This ZIP exceeds the ${MAX_LOCAL_IMPORT_FILES.toLocaleString()} entry import limit.`
        )
      )
      return
    }

    const path = normalizedWorkspaceImportPath(entry.name)
    if (!path) {
      fail(new Error(`The ZIP contains an unsafe path: “${entry.name}”.`))
      return
    }

    const isDirectory = entry.name.endsWith('/')
    const shouldSkip =
      isDirectory ||
      isSensitiveLocalImportPath(path) ||
      (entry.originalSize !== undefined &&
        entry.originalSize > MAX_LOCAL_IMPORT_FILE_BYTES)
    const chunks: ArrayBuffer[] = []
    let fileBytes = 0
    activeFiles.add(entry)
    entry.ondata = (error, chunk, final) => {
      if (error) {
        fail(error)
        return
      }
      if (failure) return

      extractedBytes += chunk.byteLength
      if (extractedBytes > MAX_LOCAL_IMPORT_BYTES) {
        fail(new Error('The expanded ZIP exceeds the 256 MB import limit.'))
        return
      }
      if (!shouldSkip) {
        fileBytes += chunk.byteLength
        if (fileBytes > MAX_LOCAL_IMPORT_FILE_BYTES) {
          fail(
            new Error(
              `The ZIP file “${path}” exceeds the ${MAX_LOCAL_IMPORT_FILE_BYTES / (1024 * 1024)} MB file limit.`
            )
          )
          return
        }
        if (chunk.byteLength > 0) chunks.push(chunk.slice().buffer)
      }
      if (!final) return

      activeFiles.delete(entry)
      if (isDirectory) return
      if (shouldSkip) {
        skipped.push(path)
        return
      }
      const pathParts = path.split('/')
      workerScope.postMessage(
        {
          type: 'entry',
          name: pathParts[pathParts.length - 1] ?? path,
          path,
          chunks,
        },
        chunks
      )
    }
    entry.start()
  })
  unzip.register(UnzipInflate)

  const reader = archive.stream().getReader()
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      for (
        let offset = 0;
        offset < value.byteLength;
        offset += MAX_COMPRESSED_CHUNK_BYTES
      ) {
        unzip.push(
          value.subarray(
            offset,
            Math.min(offset + MAX_COMPRESSED_CHUNK_BYTES, value.byteLength)
          )
        )
        if (failure) throw failure
      }
    }
    unzip.push(new Uint8Array(), true)
    if (failure) throw failure
    if (activeFiles.size > 0) {
      throw new Error('The ZIP archive ended before all entries were complete.')
    }
    workerScope.postMessage({ type: 'done', skipped })
  } catch (cause) {
    await reader.cancel(cause).catch(() => {})
    throw cause
  } finally {
    reader.releaseLock()
  }
}

workerScope.onmessage = (event) => {
  void extract(event.data.archive).catch((cause) => {
    workerScope.postMessage({ type: 'error', message: errorMessage(cause) })
  })
}
