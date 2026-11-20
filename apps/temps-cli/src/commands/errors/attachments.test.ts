// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { filenameFromDisposition, MAX_QUOTA_MIB, parseOwner, parseQuotaMib } from './attachments.js'

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
    for (const bad of ['1.5', '-1', 'lots', String(MAX_QUOTA_MIB + 1)]) {
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
