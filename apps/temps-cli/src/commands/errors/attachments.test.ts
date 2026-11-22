// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { filenameFromDisposition, MAX_QUOTA_MIB, parseOwner, parseQuotaMib, saveDownload } from './attachments.js'

describe('parseOwner', () => {
  test('an event needs its group, a feedback stands alone', () => {
    expect(parseOwner({ groupId: '2', eventId: '9' })).toEqual({ kind: 'event', groupId: 2, eventId: 9 })
    expect(parseOwner({ feedbackId: '4' })).toEqual({ kind: 'feedback', feedbackId: 4 })
  })

  test('refuses neither, both, a missing half or a bad id, naming the flag', () => {
    expect(() => parseOwner({})).toThrow('either --group-id with --event-id, or --feedback-id')
    expect(() => parseOwner({ groupId: '2', eventId: '9', feedbackId: '4' })).toThrow('either')
    expect(() => parseOwner({ groupId: '2' })).toThrow('--event-id')
    expect(() => parseOwner({ feedbackId: 'x' })).toThrow('--feedback-id must be a positive whole number')
  })
})

describe('parseQuotaMib', () => {
  test('accepts 0 to 100 GiB in whole MiB', () => {
    expect(parseQuotaMib('0')).toBe(0)
    expect(parseQuotaMib(String(MAX_QUOTA_MIB))).toBe(MAX_QUOTA_MIB)
  })

  test('refuses fractions, negatives, junk and too much, naming the flag', () => {
    for (const bad of ['1.5', '-1', 'lots', '', ' ', '1e3', '0x10', String(MAX_QUOTA_MIB + 1)]) {
      expect(() => parseQuotaMib(bad)).toThrow('--mib')
    }
  })
})

describe('filenameFromDisposition', () => {
  test('prefers the exact encoded name', () => {
    expect(
      filenameFromDisposition(
        "attachment; filename=\"captura _.png\"; filename*=UTF-8''captura%20%C3%B1.png",
        'attachment-1',
      ),
    ).toBe('captura ñ.png')
  })

  test('never writes outside the current directory', () => {
    expect(filenameFromDisposition("attachment; filename*=UTF-8''..%2F..%2Fetc%2Fpasswd", 'x')).toBe('passwd')
    expect(filenameFromDisposition('attachment; filename="..\\\\secret.txt"', 'x')).toBe('secret.txt')
    expect(filenameFromDisposition('attachment; filename=".."', 'attachment-7')).toBe('attachment-7')
    expect(filenameFromDisposition(null, 'attachment-7')).toBe('attachment-7')
  })

  test('a malformed encoding falls back to the quoted name', () => {
    expect(filenameFromDisposition("inline; filename=\"shot.png\"; filename*=UTF-8''%E0%A4%A", 'x')).toBe('shot.png')
  })
})

describe('saveDownload', () => {
  test('creates a new file, never replaces one without force, and replaces it with force', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'temps-attachment-'))
    try {
      const path = join(dir, 'app.log')
      expect(await saveDownload(path, new TextEncoder().encode('first'), false)).toBe(true)
      expect(await saveDownload(path, new TextEncoder().encode('second'), false)).toBe(false)
      expect(await Bun.file(path).text()).toBe('first')
      expect(await saveDownload(path, new TextEncoder().encode('third'), true)).toBe(true)
      expect(await Bun.file(path).text()).toBe('third')
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })

  test('creates a missing output directory without force, as it does with force', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'temps-attachment-'))
    try {
      const path = join(dir, 'new-dir', 'nested', 'app.log')
      expect(await saveDownload(path, new TextEncoder().encode('first'), false)).toBe(true)
      expect(await Bun.file(path).text()).toBe('first')
      expect(await saveDownload(path, new TextEncoder().encode('second'), false)).toBe(false)
      expect(await Bun.file(path).text()).toBe('first')
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })

  test('of two concurrent downloads to one name without force, exactly one writes', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'temps-attachment-'))
    try {
      const path = join(dir, 'app.log')
      const results = await Promise.all(
        ['a', 'b'].map((body) => saveDownload(path, new TextEncoder().encode(body), false)),
      )
      expect(results.filter(Boolean)).toHaveLength(1)
      expect(['a', 'b']).toContain(await Bun.file(path).text())
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })
})
