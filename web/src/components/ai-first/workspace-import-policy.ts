// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const MAX_LOCAL_IMPORT_FILES = 5_000
export const MAX_LOCAL_IMPORT_BYTES = 256 * 1024 * 1024
export const MAX_WRITE_BATCH_BYTES = 4 * 1024 * 1024
export const MAX_LOCAL_IMPORT_FILE_BYTES = MAX_WRITE_BATCH_BYTES
export const MAX_WRITE_BATCH_FILES = 32
export const MAX_LOCAL_IMPORT_PATH_BYTES = 512

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
  '.temps',
  '.terraform',
  '.turbo',
  '.vercel',
  '__macosx',
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
  '.yarnrc',
  '.ds_store',
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

export function normalizedWorkspaceImportPath(path: string): string | null {
  const normalized = path.replace(/\\/g, '/')
  if (normalized.startsWith('/') || normalized.includes('\0')) return null
  const parts = normalized.split('/').filter(Boolean)
  if (
    parts.length === 0 ||
    parts.some((part) => part === '.' || part === '..')
  ) {
    return null
  }
  return parts.join('/')
}

export function isSensitiveLocalImportPath(path: string): boolean {
  const parts = path.split('/').map((part) => part.toLowerCase())
  if (parts.some((part) => IGNORED_DIRECTORY_NAMES.has(part))) return true
  const fileName = parts[parts.length - 1]?.toLowerCase() ?? ''
  if (
    fileName === '.env' ||
    (fileName.startsWith('.env.') && fileName !== '.env.example')
  )
    return true
  if (SENSITIVE_FILE_NAMES.has(fileName)) return true
  if (fileName.startsWith('._')) return true
  if (
    fileName.endsWith('.credentials.json') ||
    (fileName.includes('service-account') && fileName.endsWith('.json')) ||
    (parts.includes('.config') && parts.includes('gcloud'))
  )
    return true
  return SENSITIVE_SUFFIXES.some((suffix) => fileName.endsWith(suffix))
}

export function shouldSkipLocalImportPath(path: string): boolean {
  return (
    new TextEncoder().encode(path).byteLength > MAX_LOCAL_IMPORT_PATH_BYTES ||
    isSensitiveLocalImportPath(path)
  )
}
