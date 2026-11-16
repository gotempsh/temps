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

/**
 * Operator ceilings for sandboxes created through the API: setting key, label,
 * allowed range, whether fractions are allowed and what an empty box means.
 * The ranges are the server's (`validate_sandbox_limits`) and its built-in
 * ceilings; a value above a built-in ceiling would have no effect.
 */
export const SANDBOX_API_LIMIT_FIELDS = [
  {
    key: 'max_cpu_limit',
    label: 'Max CPU cores per sandbox',
    min: 0.01,
    max: 16,
    integer: false,
    empty: 'Built-in maximum (16)',
  },
  {
    key: 'max_memory_limit_mb',
    label: 'Max memory per sandbox (MB)',
    min: 6,
    max: 32768,
    integer: true,
    empty: 'Built-in maximum (32768)',
  },
  {
    key: 'max_pids_limit',
    label: 'Max processes per sandbox',
    min: 1,
    max: 4096,
    integer: true,
    empty: 'Built-in maximum (4096)',
  },
  {
    key: 'max_sandboxes_per_user',
    label: 'Max sandboxes per user',
    min: 1,
    max: 100000,
    integer: true,
    empty: 'No cap',
  },
] as const

export type SandboxApiLimitKey =
  (typeof SANDBOX_API_LIMIT_FIELDS)[number]['key']

/** What the form holds while editing: an empty box means "no operator limit". */
export type SandboxApiLimitsDraft = Record<
  SandboxApiLimitKey,
  number | undefined
>

/** What is saved: `null` clears the limit. */
export type SandboxApiLimits = Record<SandboxApiLimitKey, number | null>

export type SandboxApiLimitsResult =
  { ok: true; limits: SandboxApiLimits } | { ok: false; message: string }

/** Unlike the resource boxes above, an empty box here is valid. */
export function validateSandboxApiLimits(
  draft: SandboxApiLimitsDraft
): SandboxApiLimitsResult {
  const limits = {} as SandboxApiLimits
  for (const field of SANDBOX_API_LIMIT_FIELDS) {
    const value = draft[field.key]
    if (value !== undefined) {
      const error = rangeError(
        field.label,
        value,
        field.min,
        field.max,
        field.integer
      )
      if (error) return { ok: false, message: error }
    }
    limits[field.key] = value ?? null
  }
  return { ok: true, limits }
}
