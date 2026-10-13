// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The editable workspace file limits: setting key, label and allowed range. */
export const WORKSPACE_FILE_LIMIT_FIELDS = [
  ['max_files_per_upload', 'Files per upload', 1, 100],
  ['max_file_size_mb', 'File size (MB)', 1, 32],
  ['max_upload_size_mb', 'Upload batch (MB)', 1, 32],
  ['max_workspace_size_mb', 'Workspace size (MB)', 1, 2048],
  ['max_workspace_entries', 'Workspace entries', 1, 50000],
  ['max_text_preview_kb', 'Text preview (KB)', 1, 1024],
  ['max_image_preview_size_mb', 'Image preview (MB)', 1, 16],
  ['max_download_size_mb', 'Download size (MB)', 1, 32],
] as const

export type WorkspaceFileLimitKey =
  (typeof WORKSPACE_FILE_LIMIT_FIELDS)[number][0]

export type WorkspaceFileLimits = Record<WorkspaceFileLimitKey, number>

/** What the form holds while editing: a box may be empty. */
export type WorkspaceFileLimitsDraft = Record<
  WorkspaceFileLimitKey,
  number | undefined
>

export const CPU_LIMIT_RANGE = { min: 0.5, max: 16 } as const
export const MEMORY_LIMIT_RANGE = { min: 256, max: 32768 } as const

export interface SandboxLimitsDraft {
  cpuLimit: number | undefined
  memoryLimitMb: number | undefined
  workspaceFileLimits: WorkspaceFileLimitsDraft
}

export type SandboxLimitsResult =
  | {
      ok: true
      cpuLimit: number
      memoryLimitMb: number
      workspaceFileLimits: WorkspaceFileLimits
    }
  | { ok: false; message: string }

function rangeError(
  label: string,
  value: number | undefined,
  min: number,
  max: number,
  integer: boolean
): string | null {
  if (value === undefined) return `${label} is required`
  if (integer && !Number.isInteger(value)) {
    return `${label} must be a whole number`
  }
  if (value < min || value > max) {
    return `${label} must be between ${min} and ${max}`
  }
  return null
}

/**
 * Checks the sandbox resource boxes on save. The boxes may be empty or
 * out of range while the user is typing; that is reported here, with the
 * field's name and allowed range, instead of silently rewriting the value.
 */
export function validateSandboxLimits(
  draft: SandboxLimitsDraft
): SandboxLimitsResult {
  const cpuError = rangeError(
    'CPU cores',
    draft.cpuLimit,
    CPU_LIMIT_RANGE.min,
    CPU_LIMIT_RANGE.max,
    false
  )
  if (cpuError) return { ok: false, message: cpuError }
  const memoryError = rangeError(
    'Memory (MB)',
    draft.memoryLimitMb,
    MEMORY_LIMIT_RANGE.min,
    MEMORY_LIMIT_RANGE.max,
    true
  )
  if (memoryError) return { ok: false, message: memoryError }

  const workspaceFileLimits = {} as WorkspaceFileLimits
  for (const [key, label, min, max] of WORKSPACE_FILE_LIMIT_FIELDS) {
    const value = draft.workspaceFileLimits[key]
    const error = rangeError(label, value, min, max, true)
    if (error) return { ok: false, message: error }
    workspaceFileLimits[key] = value as number
  }
  return {
    ok: true,
    cpuLimit: draft.cpuLimit as number,
    memoryLimitMb: draft.memoryLimitMb as number,
    workspaceFileLimits,
  }
}
