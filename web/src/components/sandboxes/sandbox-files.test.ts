// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  decodeFile,
  defaultFilePath,
  encodeText,
  fileModeArgv,
  filesUnavailableReason,
  formatFileMode,
  parseFileMode,
  isEditableFile,
  MAX_EDITABLE_BYTES,
  validateSandboxPath,
} from './sandbox-files'

describe('validateSandboxPath', () => {
  test('accepts an absolute file path', () => {
    expect(validateSandboxPath('/home/user/app/index.ts')).toBeNull()
  })

  test('explains each way a path is unusable', () => {
    expect(validateSandboxPath('  ')).toContain('Enter')
    expect(validateSandboxPath('app/index.ts')).toContain('absolute')
    expect(validateSandboxPath('/home/user/')).toContain('directory')
    expect(validateSandboxPath('/a\0b')).toContain('invalid')
    expect(validateSandboxPath(`/${'a'.repeat(5000)}`)).toContain('too long')
  })
})

describe('defaultFilePath', () => {
  test('starts the path in the work dir', () => {
    expect(defaultFilePath('/home/user/work')).toBe('/home/user/work/')
    expect(defaultFilePath('/home/user/work/')).toBe('/home/user/work/')
  })

  test('falls back to the root for a missing or relative work dir', () => {
    expect(defaultFilePath(undefined)).toBe('/')
    expect(defaultFilePath('workspace')).toBe('/')
  })
})

describe('encodeText / decodeFile', () => {
  test('round-trips ASCII and non-Latin-1 text', () => {
    for (const text of ['hello\n', 'héllo — 日本語 🚀', '']) {
      expect(decodeFile(encodeText(text))).toEqual({ ok: true, text })
    }
  })

  test('round-trips text larger than one encoding chunk', () => {
    const text = 'x'.repeat(100_000)
    expect(decodeFile(encodeText(text))).toEqual({ ok: true, text })
  })

  test('refuses binary content and invalid base64', () => {
    expect(decodeFile(btoa(String.fromCharCode(0x89, 0x50, 0, 0x47)))).toEqual({
      ok: false,
      reason: 'binary',
    })
    expect(decodeFile(btoa(String.fromCharCode(0xff, 0xfe, 0xfd)))).toEqual({
      ok: false,
      reason: 'binary',
    })
    expect(decodeFile('***not base64***')).toEqual({
      ok: false,
      reason: 'invalid',
    })
  })
})

describe('filesUnavailableReason', () => {
  test('is null while running and for a sleeping workspace', () => {
    expect(filesUnavailableReason({ status: 'running' })).toBeNull()
    expect(
      filesUnavailableReason({ status: 'stopped', lifecycle: 'workspace' })
    ).toBeNull()
  })

  test('explains stopped and destroyed sandboxes', () => {
    expect(filesUnavailableReason({ status: 'stopped' })).toContain('Resume')
    expect(filesUnavailableReason({ status: 'destroyed' })).toContain(
      'destroyed'
    )
  })
})

describe('isEditableFile', () => {
  const file = { exists: true, is_file: true, size: 10 }
  test('accepts a small regular file', () => {
    expect(isEditableFile(file)).toBe(true)
    expect(isEditableFile({ ...file, size: MAX_EDITABLE_BYTES })).toBe(true)
  })
  test('refuses missing, non-regular, oversized and unknown paths', () => {
    expect(isEditableFile({ ...file, exists: false })).toBe(false)
    expect(isEditableFile({ ...file, is_file: false })).toBe(false)
    expect(isEditableFile({ ...file, size: MAX_EDITABLE_BYTES + 1 })).toBe(
      false
    )
    expect(isEditableFile(undefined)).toBe(false)
  })
})

describe('file mode helpers', () => {
  test('stat is asked for the octal permission bits, after --', () => {
    expect(fileModeArgv('/a b/run.sh')).toEqual([
      'stat',
      '-c',
      '%a',
      '--',
      '/a b/run.sh',
    ])
  })

  test('parses stat output into the numeric mode', () => {
    expect(parseFileMode('755\n')).toBe(0o755)
    expect(parseFileMode('0644')).toBe(0o644)
    expect(parseFileMode('4755\n')).toBe(0o4755)
    expect(parseFileMode('600')).toBe(0o600)
  })

  test('rejects anything that is not an octal mode', () => {
    expect(parseFileMode('')).toBeNull()
    expect(parseFileMode('stat: cannot stat')).toBeNull()
    expect(parseFileMode('999')).toBeNull()
    expect(parseFileMode('75')).toBeNull()
    expect(parseFileMode('12345')).toBeNull()
  })

  test('formats a mode the way chmod users write it', () => {
    expect(formatFileMode(0o755)).toBe('0755')
    expect(formatFileMode(0o600)).toBe('0600')
    expect(formatFileMode(0o4755)).toBe('4755')
    expect(formatFileMode(0)).toBe('0000')
  })
})
