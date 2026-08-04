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
  file(success: (file: File) => void, error: (error: DOMException) => void): void
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

/**
 * `readEntries` returns at most ~100 entries per call and signals completion
 * with an empty batch, so a large directory needs to be drained in a loop.
 */
async function readDirectoryEntries(
  directory: LegacyDirectoryEntry
): Promise<LegacyFileSystemEntry[]> {
  const reader = directory.createReader()
  const entries: LegacyFileSystemEntry[] = []
  while (true) {
    const batch = await new Promise<LegacyFileSystemEntry[]>((resolve, reject) =>
      reader.readEntries(resolve, reject)
    )
    if (batch.length === 0) return entries
    entries.push(...batch)
  }
}

async function readEntry(
  entry: LegacyFileSystemEntry,
  prefix = ''
): Promise<DropFile[]> {
  const path = prefix ? `${prefix}/${entry.name}` : entry.name
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) =>
      (entry as LegacyFileEntry).file(resolve, reject)
    )
    return [{ file, path }]
  }
  if (!entry.isDirectory) return []

  const children = await readDirectoryEntries(entry as LegacyDirectoryEntry)
  const nested = await Promise.all(children.map((child) => readEntry(child, path)))
  return nested.flat()
}

/** Read a drop event into `DropFile[]`, recursing into dropped directories. */
export async function filesFromDrop(
  event: React.DragEvent
): Promise<DropFile[]> {
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
    return (await Promise.all(entryItems.map((entry) => readEntry(entry)))).flat()
  }

  return Array.from(event.dataTransfer.files).map((file) => ({
    file,
    path: file.webkitRelativePath || file.name,
  }))
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
