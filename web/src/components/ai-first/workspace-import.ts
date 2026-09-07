// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type WorkspaceSourceMode = 'blank' | 'local' | 'git'

export const MAX_LOCAL_IMPORT_FILES = 5_000
export const MAX_LOCAL_IMPORT_BYTES = 256 * 1024 * 1024
export const MAX_LOCAL_IMPORT_FILE_BYTES = 32 * 1024 * 1024
export const MAX_WRITE_BATCH_BYTES = 4 * 1024 * 1024
export const MAX_WRITE_BATCH_FILES = 32

const IGNORED_DIRECTORY_NAMES = new Set([
  '.aws',
  '.azure',
  '.docker',
  '.gnupg',
  '.git',
  '.kube',
  '.next',
  '.pulumi',
  '.ssh',
  '.terraform',
  '.turbo',
  '.vercel',
  'build',
  'coverage',
  'dist',
  'node_modules',
  'target',
])

const SENSITIVE_FILE_NAMES = new Set([
  '.netrc',
  '.npmrc',
  '.pypirc',
  '.envrc',
  '.git-credentials',
  'credentials',
  'credentials.json',
  'id_dsa',
  'id_ed25519',
  'id_rsa',
])

const SENSITIVE_SUFFIXES = [
  '.jks',
  '.key',
  '.keystore',
  '.p12',
  '.pfx',
  '.pem',
  '.tfstate',
  '.tfstate.backup',
]

export type LocalImportFile = {
  file: File
  path: string
}

export type LocalImportSelection = {
  accepted: LocalImportFile[]
  skipped: string[]
  totalBytes: number
  rootName: string | null
}

function normalizedRelativePath(file: File): {
  path: string
  rootName: string | null
} | null {
  const browserPath = file.webkitRelativePath || file.name
  const rawParts = browserPath.replace(/\\/g, '/').split('/')
  const parts = rawParts.filter(Boolean)
  const rootName = parts.length > 1 ? (parts.shift() ?? null) : null
  if (
    parts.length === 0 ||
    parts.some((part) => part === '.' || part === '..' || part.includes('\0'))
  ) {
    return null
  }
  return { path: parts.join('/'), rootName }
}

export function shouldSkipLocalImportPath(path: string): boolean {
  const parts = path.split('/').map((part) => part.toLowerCase())
  if (parts.some((part) => IGNORED_DIRECTORY_NAMES.has(part))) return true
  const fileName = parts[parts.length - 1]?.toLowerCase() ?? ''
  if (fileName === '.env' || fileName.startsWith('.env.')) return true
  if (SENSITIVE_FILE_NAMES.has(fileName)) return true
  if (
    fileName.endsWith('.credentials.json') ||
    (fileName.includes('service-account') && fileName.endsWith('.json')) ||
    (parts.includes('.config') && parts.includes('gcloud'))
  )
    return true
  return SENSITIVE_SUFFIXES.some((suffix) => fileName.endsWith(suffix))
}

export function prepareLocalImport(files: File[]): LocalImportSelection {
  const accepted: LocalImportFile[] = []
  const skipped: string[] = []
  let totalBytes = 0
  let rootName: string | null = null

  for (const file of files) {
    const normalized = normalizedRelativePath(file)
    if (!normalized) {
      skipped.push(file.webkitRelativePath || file.name)
      continue
    }
    rootName ??= normalized.rootName
    if (
      shouldSkipLocalImportPath(normalized.path) ||
      file.size > MAX_LOCAL_IMPORT_FILE_BYTES
    ) {
      skipped.push(normalized.path)
      continue
    }
    if (accepted.length >= MAX_LOCAL_IMPORT_FILES) {
      throw new Error(
        `This folder exceeds the ${MAX_LOCAL_IMPORT_FILES.toLocaleString()} file import limit.`
      )
    }
    if (totalBytes + file.size > MAX_LOCAL_IMPORT_BYTES) {
      throw new Error('This folder exceeds the 256 MB import limit.')
    }
    totalBytes += file.size
    accepted.push({ file, path: normalized.path })
  }

  if (accepted.length === 0) {
    throw new Error('The selected folder contains no importable files.')
  }
  return { accepted, skipped, totalBytes, rootName }
}

export function batchLocalImportFiles(
  files: LocalImportFile[]
): LocalImportFile[][] {
  const batches: LocalImportFile[][] = []
  let batch: LocalImportFile[] = []
  let batchBytes = 0
  for (const file of files) {
    if (
      batch.length > 0 &&
      (batch.length >= MAX_WRITE_BATCH_FILES ||
        batchBytes + file.file.size > MAX_WRITE_BATCH_BYTES)
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
