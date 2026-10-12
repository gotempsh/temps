// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  validateSandboxLimits,
  type SandboxLimitsDraft,
  type WorkspaceFileLimits,
  type WorkspaceFileLimitsDraft,
} from './sandbox-limits'

const LIMITS: WorkspaceFileLimitsDraft = {
  max_files_per_upload: 32,
  max_file_size_mb: 16,
  max_upload_size_mb: 32,
  max_workspace_size_mb: 256,
  max_workspace_entries: 5000,
  max_text_preview_kb: 256,
  max_image_preview_size_mb: 8,
  max_download_size_mb: 32,
}

const VALID: SandboxLimitsDraft = {
  cpuLimit: 4,
  memoryLimitMb: 8192,
  workspaceFileLimits: LIMITS,
}

test('accepts the defaults and returns plain numbers', () => {
  const result = validateSandboxLimits(VALID)
  expect(result).toEqual({
    ok: true,
    cpuLimit: 4,
    memoryLimitMb: 8192,
    workspaceFileLimits: LIMITS as WorkspaceFileLimits,
  })
})

test('an emptied box is a required-field error that names the field', () => {
  expect(validateSandboxLimits({ ...VALID, cpuLimit: undefined })).toEqual({
    ok: false,
    message: 'CPU cores is required',
  })
  expect(
    validateSandboxLimits({
      ...VALID,
      workspaceFileLimits: { ...LIMITS, max_file_size_mb: undefined },
    })
  ).toEqual({ ok: false, message: 'File size (MB) is required' })
})

test('min and max are still enforced, with the allowed range in the message', () => {
  expect(validateSandboxLimits({ ...VALID, cpuLimit: 0.25 })).toEqual({
    ok: false,
    message: 'CPU cores must be between 0.5 and 16',
  })
  expect(validateSandboxLimits({ ...VALID, memoryLimitMb: 99999 })).toEqual({
    ok: false,
    message: 'Memory (MB) must be between 256 and 32768',
  })
  expect(
    validateSandboxLimits({
      ...VALID,
      workspaceFileLimits: { ...LIMITS, max_upload_size_mb: 33 },
    })
  ).toEqual({
    ok: false,
    message: 'Upload batch (MB) must be between 1 and 32',
  })
})

test('whole-number boxes reject fractions, fractional cores are allowed', () => {
  expect(validateSandboxLimits({ ...VALID, cpuLimit: 1.5 }).ok).toBe(true)
  expect(validateSandboxLimits({ ...VALID, memoryLimitMb: 512.5 })).toEqual({
    ok: false,
    message: 'Memory (MB) must be a whole number',
  })
})
