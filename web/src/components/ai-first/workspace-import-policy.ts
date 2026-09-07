// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const MAX_LOCAL_IMPORT_FILES = 5_000
export const MAX_LOCAL_IMPORT_BYTES = 256 * 1024 * 1024
export const MAX_WRITE_BATCH_BYTES = 32 * 1024 * 1024
export const MAX_LOCAL_IMPORT_FILE_BYTES = 16 * 1024 * 1024
export const MAX_WRITE_BATCH_FILES = 32
export const MAX_LOCAL_IMPORT_PATH_BYTES = 512

export type WorkspaceImportLimits = {
  maxFiles: number
  maxBytes: number
  maxFileBytes: number
  maxBatchBytes: number
  maxBatchFiles: number
  maxTextPreviewKb: number
  maxImagePreviewMb: number
}

export const DEFAULT_WORKSPACE_IMPORT_LIMITS: WorkspaceImportLimits = {
  maxFiles: MAX_LOCAL_IMPORT_FILES,
  maxBytes: MAX_LOCAL_IMPORT_BYTES,
  maxFileBytes: MAX_LOCAL_IMPORT_FILE_BYTES,
  maxBatchBytes: MAX_WRITE_BATCH_BYTES,
  maxBatchFiles: MAX_WRITE_BATCH_FILES,
  maxTextPreviewKb: 256,
  maxImagePreviewMb: 8,
}

export function workspaceImportLimitsFromSettings(settings?: {
  max_files_per_upload?: number
  max_file_size_mb?: number
  max_upload_size_mb?: number
  max_workspace_size_mb?: number
  max_workspace_entries?: number
  max_text_preview_kb?: number
  max_image_preview_size_mb?: number
}): WorkspaceImportLimits {
  if (!settings) return DEFAULT_WORKSPACE_IMPORT_LIMITS
  return {
    maxFiles: Math.min(
      settings.max_workspace_entries ?? MAX_LOCAL_IMPORT_FILES,
      MAX_LOCAL_IMPORT_FILES
    ),
    // Browser selection and ZIP extraction keep an independent safety budget.
    // The persistent workspace quota may be much larger and must never become
    // permission to materialize gigabytes of attacker-controlled data in a tab.
    maxBytes: Math.min(
      (settings.max_workspace_size_mb ?? 256) * 1024 * 1024,
      MAX_LOCAL_IMPORT_BYTES
    ),
    maxFileBytes: Math.min(
      (settings.max_file_size_mb ?? 16) * 1024 * 1024,
      MAX_LOCAL_IMPORT_FILE_BYTES
    ),
    maxBatchBytes: Math.min(
      (settings.max_upload_size_mb ?? 32) * 1024 * 1024,
      MAX_WRITE_BATCH_BYTES
    ),
    maxBatchFiles: Math.min(
      settings.max_files_per_upload ?? MAX_WRITE_BATCH_FILES,
      MAX_WRITE_BATCH_FILES
    ),
    maxTextPreviewKb: settings.max_text_preview_kb ?? 256,
    maxImagePreviewMb: settings.max_image_preview_size_mb ?? 8,
  }
}

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
