// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DropFile } from '@/lib/drop-archive'

/**
 * Browser plumbing for turning a drag-and-drop or file-picker gesture into a
 * flat `DropFile[]`, shared by the standalone `/drop` page and the
 * project-scoped `/projects/:slug/drop` page.
 *
 * Folder drops only work through the non-standard `webkitGetAsEntry` API —
 * `dataTransfer.files` flattens a directory to nothing. Every browser Temps
 * supports implements it, but it is not in the DOM lib types, so the minimal
 * `Legacy*` shapes below describe just the parts we call.
 */

interface LegacyFileSystemEntry {
  isFile: boolean
  isDirectory: boolean
  name: string
}

interface LegacyFileEntry extends LegacyFileSystemEntry {
  file(
    success: (file: File) => void,
    error: (error: DOMException) => void
  ): void
}

interface LegacyDirectoryReader {
  readEntries(
    success: (entries: LegacyFileSystemEntry[]) => void,
    error: (error: DOMException) => void
  ): void
}

interface LegacyDirectoryEntry extends LegacyFileSystemEntry {
  createReader(): LegacyDirectoryReader
}

export interface DropReadOptions {
  maxEntries?: number
  signal?: AbortSignal
  shouldSkipPath?: (path: string, isDirectory: boolean) => boolean
  onSkippedPath?: (path: string) => void
}

interface DropReadState {
  entryCount: number
  options: DropReadOptions
}

async function readEntry(
  entry: LegacyFileSystemEntry,
  state: DropReadState,
  prefix = ''
): Promise<DropFile[]> {
  state.options.signal?.throwIfAborted()
  const path = prefix ? `${prefix}/${entry.name}` : entry.name
  state.entryCount += 1
  if (
    state.options.maxEntries !== undefined &&
    state.entryCount > state.options.maxEntries
  ) {
    throw new Error(
      `The dropped selection exceeds the ${state.options.maxEntries.toLocaleString()} entry reading limit.`
    )
  }
  if (state.options.shouldSkipPath?.(path, entry.isDirectory)) {
    state.options.onSkippedPath?.(path)
    return []
  }
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) =>
      (entry as LegacyFileEntry).file(resolve, reject)
    )
    state.options.signal?.throwIfAborted()
    return [{ file, path }]
  }
  if (!entry.isDirectory) return []

  // Chromium returns directory entries in batches of roughly 100. Process
  // each batch before reading the next one so limits and ignored directories
  // apply without materializing an unbounded tree in memory.
  const reader = (entry as LegacyDirectoryEntry).createReader()
  const nested: DropFile[] = []
  while (true) {
    const children = await new Promise<LegacyFileSystemEntry[]>(
      (resolve, reject) => reader.readEntries(resolve, reject)
    )
    state.options.signal?.throwIfAborted()
    if (children.length === 0) return nested
    for (const child of children) {
      nested.push(...(await readEntry(child, state, path)))
    }
  }
}

/** Read a drop event into `DropFile[]`, recursing into dropped directories. */
export async function filesFromDrop(
  event: React.DragEvent,
  options: DropReadOptions = {}
): Promise<DropFile[]> {
  options.signal?.throwIfAborted()
  const items = Array.from(event.dataTransfer.items)
  const entryItems = items
    .map((item) => {
      const getEntry = (
        item as DataTransferItem & {
          webkitGetAsEntry?: () => LegacyFileSystemEntry | null
        }
      ).webkitGetAsEntry
      return getEntry?.call(item) ?? null
    })
    .filter((entry): entry is LegacyFileSystemEntry => entry !== null)

  if (entryItems.length > 0) {
    const state: DropReadState = { entryCount: 0, options }
    const files: DropFile[] = []
    for (const entry of entryItems) {
      files.push(...(await readEntry(entry, state)))
    }
    return files
  }

  const files = Array.from(event.dataTransfer.files)
  const selected: DropFile[] = []
  for (const file of files) {
    options.signal?.throwIfAborted()
    if (
      options.maxEntries !== undefined &&
      selected.length >= options.maxEntries
    ) {
      throw new Error(
        `The dropped selection exceeds the ${options.maxEntries.toLocaleString()} entry reading limit.`
      )
    }
    const path = file.webkitRelativePath || file.name
    if (options.shouldSkipPath?.(path, false)) {
      options.onSkippedPath?.(path)
      continue
    }
    selected.push({ file, path })
  }
  return selected
}

/** Read an `<input type="file">` selection into `DropFile[]`. */
export function filesFromInput(selected: FileList | null): DropFile[] {
  if (!selected) return []
  return Array.from(selected).map((file) => ({
    file,
    path: file.webkitRelativePath || file.name,
  }))
}

/** Best-effort project name from the dropped folder or archive name. */
export function inferredProjectName(files: DropFile[]): string {
  const firstPath = files[0]?.path.replace(/\\/g, '/') || ''
  const parts = firstPath.split('/').filter(Boolean)
  const source = parts.length > 1 ? parts[0] : parts[0] || ''
  return source
    .replace(/\.(tar\.gz|tgz|zip|tar|html?)$/i, '')
    .replace(/[_-]+/g, ' ')
    .trim()
}

/**
 * Unwrap whatever the API client threw into something worth showing a user.
 * Problem Details put the useful sentence in `detail`, not `message`.
 */
export function dropErrorMessage(
  error: unknown,
  fallback = 'The drop could not be deployed'
): string {
  if (error instanceof Error) return error.message
  if (error && typeof error === 'object') {
    const problem = error as { detail?: string; message?: string }
    return problem.detail || problem.message || fallback
  }
  return fallback
}
