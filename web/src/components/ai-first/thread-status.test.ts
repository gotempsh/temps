// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { threadDisplayStatus } from './thread-status'

describe('threadDisplayStatus', () => {
  test('shows running and untouched threads as pending', () => {
    expect(threadDisplayStatus('running')).toBe('pending')
    expect(threadDisplayStatus('idle')).toBe('pending')
    expect(threadDisplayStatus(undefined)).toBe('pending')
  })

  test('shows completed threads as succeeded', () => {
    expect(threadDisplayStatus('completed')).toBe('succeeded')
    expect(threadDisplayStatus('idle', true)).toBe('succeeded')
  })

  test('shows failed and cancelled threads as errors', () => {
    expect(threadDisplayStatus('failed')).toBe('error')
    expect(threadDisplayStatus('cancelled')).toBe('error')
  })
})
