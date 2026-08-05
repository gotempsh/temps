export interface DropFile {
  file: File
  path: string
}

export interface PreparedDrop {
  file: File
  fileCount: number
  rootPage: string | null
}

const ARCHIVE_PATTERN = /\.zip$/i
const IGNORED_PATH_PARTS = new Set([
  '.DS_Store',
  'Thumbs.db',
  '__MACOSX',
  '.git',
  'node_modules',
])
const textEncoder = new TextEncoder()
const MAX_BROWSER_PACKAGE_BYTES = 100 * 1024 * 1024

export function isDropArchive(filename: string): boolean {
  return ARCHIVE_PATTERN.test(filename)
}

export function formatDetectedProjectLabel(
  label: string,
  directory: string
): string {
  return !directory || directory === '.' ? label : `${label} · ${directory}`
}

function cleanPath(path: string): string {
  const parts = path
    .replace(/\\/g, '/')
    .split('/')
    .filter((part) => part && part !== '.')

  if (parts.some((part) => part === '..')) {
    throw new Error(`Unsafe path in dropped files: ${path}`)
  }

  return parts.join('/')
}

function shouldIgnore(path: string): boolean {
  return path
    .split('/')
    .some(
      (part) =>
        IGNORED_PATH_PARTS.has(part) ||
        part === '.env' ||
        part.startsWith('.env.') ||
        part.endsWith('.pem') ||
        part.endsWith('.key') ||
        part === 'credentials.json'
    )
}

export function normalizeDropFiles(files: DropFile[]): DropFile[] {
  const cleaned = files
    .map(({ file, path }) => ({ file, path: cleanPath(path || file.name) }))
    .filter(({ path }) => path && !shouldIgnore(path))

  if (cleaned.length === 0) return []

  const splitPaths = cleaned.map(({ path }) => path.split('/'))
  const firstPart = splitPaths[0][0]
  const hasSharedFolder = splitPaths.every(
    (parts) => parts.length > 1 && parts[0] === firstPart
  )

  return hasSharedFolder
    ? cleaned.map(({ file, path }) => ({
        file,
        path: path.slice(firstPart.length + 1),
      }))
    : cleaned
}

export function htmlRootCandidates(files: DropFile[]): string[] {
  return normalizeDropFiles(files)
    .map(({ path }) => path)
    .filter((path) => path.toLowerCase().endsWith('.html'))
    .sort((a, b) => a.localeCompare(b))
}

function writeU16(view: DataView, offset: number, value: number) {
  view.setUint16(offset, value, true)
}

function writeU32(view: DataView, offset: number, value: number) {
  view.setUint32(offset, value >>> 0, true)
}

function throwIfAborted(signal?: AbortSignal) {
  if (signal?.aborted) {
    throw new DOMException('Preset detection was cancelled', 'AbortError')
  }
}

async function crc32(bytes: Uint8Array, signal?: AbortSignal): Promise<number> {
  let crc = 0xffffffff
  const yieldEveryBytes = 2 * 1024 * 1024
  for (let offset = 0; offset < bytes.length; offset += yieldEveryBytes) {
    throwIfAborted(signal)
    const end = Math.min(offset + yieldEveryBytes, bytes.length)
    for (let index = offset; index < end; index += 1) {
      crc ^= bytes[index]
      for (let bit = 0; bit < 8; bit += 1) {
        crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1))
      }
    }
    // Keep the clear button responsive while a large folder is checksummed.
    if (end < bytes.length)
      await new Promise((resolve) => setTimeout(resolve, 0))
  }
  return (crc ^ 0xffffffff) >>> 0
}

function dosTimestamp(date: Date): { date: number; time: number } {
  const year = Math.max(1980, date.getFullYear())
  return {
    date: ((year - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate(),
    time:
      (date.getHours() << 11) |
      (date.getMinutes() << 5) |
      Math.floor(date.getSeconds() / 2),
  }
}

async function createStoredZip(
  entries: Array<{ path: string; contents: Blob }>,
  signal?: AbortSignal
): Promise<Blob> {
  const localParts: BlobPart[] = []
  const centralParts: BlobPart[] = []
  let localOffset = 0
  let centralSize = 0
  const now = dosTimestamp(new Date())

  for (const entry of entries) {
    throwIfAborted(signal)
    const name = textEncoder.encode(cleanPath(entry.path))
    const contents = new Uint8Array(await entry.contents.arrayBuffer())
    const checksum = await crc32(contents, signal)

    const localHeader = new ArrayBuffer(30)
    const localView = new DataView(localHeader)
    writeU32(localView, 0, 0x04034b50)
    writeU16(localView, 4, 20)
    writeU16(localView, 6, 0x0800)
    writeU16(localView, 8, 0)
    writeU16(localView, 10, now.time)
    writeU16(localView, 12, now.date)
    writeU32(localView, 14, checksum)
    writeU32(localView, 18, contents.byteLength)
    writeU32(localView, 22, contents.byteLength)
    writeU16(localView, 26, name.byteLength)
    writeU16(localView, 28, 0)
    localParts.push(localHeader, name, contents)

    const centralHeader = new ArrayBuffer(46)
    const centralView = new DataView(centralHeader)
    writeU32(centralView, 0, 0x02014b50)
    writeU16(centralView, 4, 20)
    writeU16(centralView, 6, 20)
    writeU16(centralView, 8, 0x0800)
    writeU16(centralView, 10, 0)
    writeU16(centralView, 12, now.time)
    writeU16(centralView, 14, now.date)
    writeU32(centralView, 16, checksum)
    writeU32(centralView, 20, contents.byteLength)
    writeU32(centralView, 24, contents.byteLength)
    writeU16(centralView, 28, name.byteLength)
    writeU16(centralView, 30, 0)
    writeU16(centralView, 32, 0)
    writeU16(centralView, 34, 0)
    writeU16(centralView, 36, 0)
    writeU32(centralView, 38, 0)
    writeU32(centralView, 42, localOffset)
    centralParts.push(centralHeader, name)

    localOffset += 30 + name.byteLength + contents.byteLength
    centralSize += 46 + name.byteLength
  }

  const end = new ArrayBuffer(22)
  const endView = new DataView(end)
  writeU32(endView, 0, 0x06054b50)
  writeU16(endView, 4, 0)
  writeU16(endView, 6, 0)
  writeU16(endView, 8, entries.length)
  writeU16(endView, 10, entries.length)
  writeU32(endView, 12, centralSize)
  writeU32(endView, 16, localOffset)
  writeU16(endView, 20, 0)

  return new Blob([...localParts, ...centralParts, end], {
    type: 'application/zip',
  })
}

function redirectPage(target: string): Blob {
  const escaped = target
    .split('/')
    .map((part) => encodeURIComponent(part))
    .join('/')
  const href = `./${escaped}`
  return new Blob(
    [
      '<!doctype html><html><head><meta charset="utf-8">',
      `<meta http-equiv="refresh" content="0;url=${href}">`,
      `<title>Redirecting…</title></head><body><a href="${href}">Open site</a></body></html>`,
    ],
    { type: 'text/html' }
  )
}

export async function prepareDrop(
  rawFiles: DropFile[],
  selectedRootPage?: string,
  signal?: AbortSignal
): Promise<PreparedDrop> {
  throwIfAborted(signal)
  if (rawFiles.length === 1 && isDropArchive(rawFiles[0].file.name)) {
    if (rawFiles[0].file.size > 500 * 1024 * 1024) {
      throw new Error('Drop exceeds the 500 MB upload limit')
    }
    return { file: rawFiles[0].file, fileCount: 1, rootPage: null }
  }

  const files = normalizeDropFiles(rawFiles)
  if (files.length === 0) throw new Error('The selected folder is empty')

  const totalBytes = files.reduce((sum, { file }) => sum + file.size, 0)
  if (totalBytes > MAX_BROWSER_PACKAGE_BYTES) {
    throw new Error(
      'Folders larger than 100 MB must be zipped before upload to limit browser memory use'
    )
  }

  const paths = new Set(files.map(({ path }) => path.toLowerCase()))
  let rootPage: string | null = null
  const entries = files.map(({ file, path }) => ({
    path,
    contents: file as Blob,
  }))

  if (!paths.has('index.html')) {
    const candidates = htmlRootCandidates(files)
    rootPage = selectedRootPage || candidates[0] || null
    if (rootPage) {
      entries.push({ path: 'index.html', contents: redirectPage(rootPage) })
    }
  }

  const archive = await createStoredZip(entries, signal)
  return {
    file: new File([archive], 'temps-drop.zip', { type: 'application/zip' }),
    fileCount: files.length,
    rootPage,
  }
}
