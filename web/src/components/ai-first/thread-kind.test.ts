// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  THREAD_KINDS,
  isTerminalThread,
  terminalThreadSubtitle,
  threadKind,
} from './thread-kind'

describe('threadKind', () => {
  test('a terminal thread is a terminal', () => {
    expect(threadKind({ kind: 'terminal' })).toBe('terminal')
    expect(isTerminalThread({ kind: 'terminal' })).toBe(true)
  })

  test('everything else is an AI thread, including threads without a kind', () => {
    expect(threadKind({ kind: 'ai' })).toBe('ai')
    expect(threadKind({})).toBe('ai')
    expect(threadKind({ kind: null })).toBe('ai')
    expect(threadKind({ kind: 'notebook' })).toBe('ai')
    expect(threadKind(null)).toBe('ai')
    expect(threadKind(undefined)).toBe('ai')
    expect(isTerminalThread({})).toBe(false)
  })
})

describe('THREAD_KINDS', () => {
  test('offers AI first, then terminal, each explained', () => {
    expect(THREAD_KINDS.map((entry) => entry.kind)).toEqual(['ai', 'terminal'])
    for (const entry of THREAD_KINDS) {
      expect(entry.label.length).toBeGreaterThan(0)
      expect(entry.description.length).toBeGreaterThan(10)
    }
  })
})

describe('terminalThreadSubtitle', () => {
  test('names what the thread is, and when it is archived', () => {
    expect(terminalThreadSubtitle(false)).toBe('Terminal · workspace shell')
    expect(terminalThreadSubtitle(true)).toBe('Terminal · archived')
  })
})
