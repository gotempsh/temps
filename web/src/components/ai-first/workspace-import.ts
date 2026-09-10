// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DropFile } from '@/lib/drop-archive'
import {
  DEFAULT_WORKSPACE_IMPORT_LIMITS,
  type WorkspaceImportLimits,
  isSensitiveLocalImportPath,
  normalizedWorkspaceImportPath,
  shouldSkipLocalImportPath,
} from './workspace-import-policy'

export {
  MAX_LOCAL_IMPORT_BYTES,
  MAX_LOCAL_IMPORT_FILE_BYTES,
  MAX_LOCAL_IMPORT_FILES,
  MAX_LOCAL_IMPORT_PATH_BYTES,
  MAX_WRITE_BATCH_BYTES,
  MAX_WRITE_BATCH_FILES,
  isSensitiveLocalImportPath,
  shouldSkipLocalImportPath,
} from './workspace-import-policy'

export type WorkspaceSourceMode = 'blank' | 'local' | 'git'

const MAX_ZIP_CENTRAL_DIRECTORY_BYTES = 4 * 1024 * 1024
const MAX_ZIP_COMPRESSION_RATIO = 2_000
const MAX_ZIP_TRAILER_BYTES = 65_535 + 22
const ZIP_END_SIGNATURE = 0x06054b50
const ZIP_CENTRAL_ENTRY_SIGNATURE = 0x02014b50

export type LocalImportFile = {
  file: File
  path: string
}

export type LocalImportSelection = {
  accepted: LocalImportFile[]
  skipped: string[]
  totalBytes: number
  rootName: string | null
  sourceLabel: string
  sourceKind: 'files' | 'zip'
}

type WorkspaceImportInput = File | DropFile

function inputFile(input: WorkspaceImportInput): File {
  return input instanceof File ? input : input.file
}

function inputPath(input: WorkspaceImportInput): string {
  if (input instanceof File) {
    return input.webkitRelativePath || input.name
  }
  return input.path || input.file.webkitRelativePath || input.file.name
}

function sharedRoot(paths: string[]): string | null {
  if (paths.length === 0) return null
  const split = paths.map((path) => path.split('/'))
  const root = split[0][0]
  return split.every((parts) => parts.length > 1 && parts[0] === root)
    ? root
    : null
}

export function prepareLocalImport(
  inputs: WorkspaceImportInput[],
  options: {
    sourceKind?: LocalImportSelection['sourceKind']
    sourceLabel?: string
    skipped?: string[]
    limits?: WorkspaceImportLimits
  } = {}
): LocalImportSelection {
  const limits = options.limits ?? DEFAULT_WORKSPACE_IMPORT_LIMITS
  const accepted: LocalImportFile[] = []
  const skipped = [...(options.skipped ?? [])]
  let totalBytes = 0
  const normalized = inputs.map((input) => ({
    file: inputFile(input),
    originalPath: inputPath(input),
    path: normalizedWorkspaceImportPath(inputPath(input)),
  }))
  const rootName = sharedRoot(
    normalized
      .filter(({ path }) => path !== null)
      .map(({ path }) => path as string)
  )
  const seen = new Set<string>()

  for (const candidate of normalized) {
    if (!candidate.path) {
      skipped.push(candidate.originalPath)
      continue
    }
    const path = rootName
      ? candidate.path.slice(rootName.length + 1)
      : candidate.path
    if (
      isSensitiveLocalImportPath(candidate.path) ||
      shouldSkipLocalImportPath(path) ||
      candidate.file.size > limits.maxFileBytes
    ) {
      skipped.push(path)
      continue
    }
    if (seen.has(path)) {
      throw new Error(`The selection contains more than one file at “${path}”.`)
    }
    if (accepted.length >= limits.maxFiles) {
      throw new Error(
        `This selection exceeds the ${limits.maxFiles.toLocaleString()} file import limit.`
      )
    }
    if (totalBytes + candidate.file.size > limits.maxBytes) {
      throw new Error(
        `This selection exceeds the ${Math.floor(limits.maxBytes / (1024 * 1024))} MB import limit.`
      )
    }
    seen.add(path)
    totalBytes += candidate.file.size
    accepted.push({ file: candidate.file, path })
  }

  if (accepted.length === 0) {
    throw new Error('The selection contains no importable files.')
  }
  return {
    accepted,
    skipped,
    totalBytes,
    rootName,
    sourceLabel:
      options.sourceLabel ??
      rootName ??
      (accepted.length === 1
        ? accepted[0].file.name
        : `${accepted.length} files`),
    sourceKind: options.sourceKind ?? 'files',
  }
}

function isZip(file: File): boolean {
  return (
    file.name.toLowerCase().endsWith('.zip') ||
    file.type === 'application/zip' ||
    file.type === 'application/x-zip-compressed'
  )
}

function archiveError(cause: unknown): Error {
  return cause instanceof Error
    ? cause
    : new Error('The ZIP archive could not be read.')
}

async function validateZipCentralDirectory(
  archiveFile: File,
  limits: WorkspaceImportLimits
): Promise<void> {
  const tailStart = Math.max(0, archiveFile.size - MAX_ZIP_TRAILER_BYTES)
  const tail = new Uint8Array(await archiveFile.slice(tailStart).arrayBuffer())
  const tailView = new DataView(tail.buffer, tail.byteOffset, tail.byteLength)
  let endOffset = -1
  for (let offset = tail.byteLength - 22; offset >= 0; offset -= 1) {
    if (tailView.getUint32(offset, true) !== ZIP_END_SIGNATURE) continue
    const commentBytes = tailView.getUint16(offset + 20, true)
    if (offset + 22 + commentBytes === tail.byteLength) {
      endOffset = offset
      break
    }
  }
  if (endOffset < 0) {
    throw new Error('The selected file does not contain a valid ZIP directory.')
  }

  const disk = tailView.getUint16(endOffset + 4, true)
  const centralDisk = tailView.getUint16(endOffset + 6, true)
  const diskEntries = tailView.getUint16(endOffset + 8, true)
  const totalEntries = tailView.getUint16(endOffset + 10, true)
  const centralBytes = tailView.getUint32(endOffset + 12, true)
  const centralOffset = tailView.getUint32(endOffset + 16, true)
  if (
    disk !== 0 ||
    centralDisk !== 0 ||
    diskEntries !== totalEntries ||
    totalEntries === 0xffff ||
    centralBytes === 0xffffffff ||
    centralOffset === 0xffffffff
  ) {
    throw new Error('Multi-disk and ZIP64 archives are not supported.')
  }
  if (totalEntries > limits.maxFiles) {
    throw new Error(
      `This ZIP exceeds the ${limits.maxFiles.toLocaleString()} entry import limit.`
    )
  }
  if (centralBytes > MAX_ZIP_CENTRAL_DIRECTORY_BYTES) {
    throw new Error('The ZIP directory exceeds the 4 MB safety limit.')
  }
  const absoluteEndOffset = tailStart + endOffset
  if (
    centralOffset > absoluteEndOffset ||
    centralBytes > absoluteEndOffset - centralOffset
  ) {
    throw new Error('The ZIP directory points outside the selected archive.')
  }

  const central = new Uint8Array(
    await archiveFile
      .slice(centralOffset, centralOffset + centralBytes)
      .arrayBuffer()
  )
  const centralView = new DataView(
    central.buffer,
    central.byteOffset,
    central.byteLength
  )
  let cursor = 0
  let expandedBytes = 0
  for (let index = 0; index < totalEntries; index += 1) {
    if (
      cursor + 46 > central.byteLength ||
      centralView.getUint32(cursor, true) !== ZIP_CENTRAL_ENTRY_SIGNATURE
    ) {
      throw new Error('The ZIP central directory is malformed.')
    }
    const flags = centralView.getUint16(cursor + 8, true)
    const method = centralView.getUint16(cursor + 10, true)
    const compressedBytes = centralView.getUint32(cursor + 20, true)
    const uncompressedBytes = centralView.getUint32(cursor + 24, true)
    const nameBytes = centralView.getUint16(cursor + 28, true)
    const extraBytes = centralView.getUint16(cursor + 30, true)
    const commentBytes = centralView.getUint16(cursor + 32, true)
    if ((flags & 1) !== 0) {
      throw new Error('Encrypted ZIP archives are not supported.')
    }
    if (method !== 0 && method !== 8) {
      throw new Error(`ZIP compression method ${method} is not supported.`)
    }
    if (compressedBytes === 0xffffffff || uncompressedBytes === 0xffffffff) {
      throw new Error('ZIP64 archives are not supported.')
    }
    expandedBytes += uncompressedBytes
    if (expandedBytes > limits.maxBytes) {
      throw new Error(
        `The expanded ZIP exceeds the ${Math.floor(limits.maxBytes / (1024 * 1024))} MB import limit.`
      )
    }
    if (
      uncompressedBytes > 0 &&
      (compressedBytes === 0 ||
        uncompressedBytes / compressedBytes > MAX_ZIP_COMPRESSION_RATIO)
    ) {
      throw new Error('The ZIP contains an unsafe compression ratio.')
    }
    cursor += 46 + nameBytes + extraBytes + commentBytes
    if (cursor > central.byteLength) {
      throw new Error('The ZIP central directory is malformed.')
    }
  }
}

async function extractWorkspaceZip(
  archiveFile: File,
  limits: WorkspaceImportLimits,
  signal?: AbortSignal
): Promise<{ files: DropFile[]; skipped: string[] }> {
  signal?.throwIfAborted()
  if (archiveFile.size > limits.maxBytes) {
    throw new Error(
      `The ZIP archive exceeds the ${Math.floor(limits.maxBytes / (1024 * 1024))} MB upload limit.`
    )
  }
  await validateZipCentralDirectory(archiveFile, limits)
  signal?.throwIfAborted()

  type WorkerResponse =
    | {
        type: 'entry'
        name: string
        path: string
        chunks: ArrayBuffer[]
      }
    | { type: 'done'; skipped: string[] }
    | { type: 'error'; message: string }

  return new Promise((resolve, reject) => {
    const worker = new Worker(
      new URL('./workspace-import.worker.ts', import.meta.url),
      { name: 'temps-workspace-zip-import', type: 'module' }
    )
    const files: DropFile[] = []
    let settled = false
    const abort = () => fail(signal?.reason ?? new Error('Import cancelled.'))
    const timeout = globalThis.setTimeout(() => {
      fail(new Error('ZIP extraction exceeded the 30 second safety limit.'))
    }, 30_000)
    const cleanup = () => {
      globalThis.clearTimeout(timeout)
      signal?.removeEventListener('abort', abort)
      worker.terminate()
    }
    const fail = (cause: unknown) => {
      if (settled) return
      settled = true
      cleanup()
      reject(archiveError(cause))
    }

    worker.onerror = (event) => {
      fail(new Error(event.message || 'The ZIP archive could not be read.'))
    }
    worker.onmessage = (event: MessageEvent<WorkerResponse>) => {
      if (settled) return
      const message = event.data
      if (message.type === 'error') {
        fail(new Error(message.message))
        return
      }
      if (message.type === 'entry') {
        files.push({
          file: new File(message.chunks, message.name, {
            lastModified: archiveFile.lastModified,
          }),
          path: message.path,
        })
        return
      }
      settled = true
      cleanup()
      resolve({ files, skipped: message.skipped })
    }
    signal?.addEventListener('abort', abort, { once: true })
    if (signal?.aborted) {
      abort()
      return
    }
    worker.postMessage({ archive: archiveFile, limits })
  })
}

export async function prepareWorkspaceImport(
  inputs: DropFile[],
  options: {
    skipped?: string[]
    signal?: AbortSignal
    limits?: WorkspaceImportLimits
  } = {}
): Promise<LocalImportSelection> {
  const limits = options.limits ?? DEFAULT_WORKSPACE_IMPORT_LIMITS
  options.signal?.throwIfAborted()
  if (inputs.length === 0) {
    throw new Error('Choose a ZIP archive, files, or a folder to import.')
  }
  const selected = inputs[0]
  const selectedPath = selected
    ? normalizedWorkspaceImportPath(selected.path)
    : null
  const isTopLevelArchive =
    inputs.length === 1 &&
    selected !== undefined &&
    isZip(selected.file) &&
    selectedPath === selected.file.name
  if (isTopLevelArchive) {
    const extracted = await extractWorkspaceZip(
      selected.file,
      limits,
      options.signal
    )
    options.signal?.throwIfAborted()
    return prepareLocalImport(extracted.files, {
      skipped: [...(options.skipped ?? []), ...extracted.skipped],
      sourceKind: 'zip',
      sourceLabel: selected.file.name,
      limits,
    })
  }
  return prepareLocalImport(inputs, { skipped: options.skipped, limits })
}

export function batchLocalImportFiles(
  files: LocalImportFile[],
  limits: WorkspaceImportLimits = DEFAULT_WORKSPACE_IMPORT_LIMITS
): LocalImportFile[][] {
  const batches: LocalImportFile[][] = []
  let batch: LocalImportFile[] = []
  let batchBytes = 0
  for (const file of files) {
    if (
      batch.length > 0 &&
      (batch.length >= limits.maxBatchFiles ||
        batchBytes + file.file.size > limits.maxBatchBytes)
    ) {
      batches.push(batch)
      batch = []
      batchBytes = 0
    }
    batch.push(file)
    batchBytes += file.file.size
  }
  if (batch.length > 0) batches.push(batch)
  return batches
}

export async function fileToBase64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer())
  const chunks: string[] = []
  const chunkSize = 0x8000
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    chunks.push(
      String.fromCharCode(...bytes.subarray(offset, offset + chunkSize))
    )
  }
  return btoa(chunks.join(''))
}
