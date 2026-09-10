// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { httpStatusClass, isHttpErrorStatus } from './http-status-class'

describe('httpStatusClass', () => {
  it('classifies 101 Switching Protocols as informational, not an error', () => {
    expect(httpStatusClass(101)).toBe('1xx')
  })

  it('classifies other 1xx codes as informational', () => {
    expect(httpStatusClass(100)).toBe('1xx')
    expect(httpStatusClass(103)).toBe('1xx')
  })

  it('classifies 2xx as success', () => {
    expect(httpStatusClass(200)).toBe('2xx')
    expect(httpStatusClass(299)).toBe('2xx')
  })

  it('classifies 3xx as redirect', () => {
    expect(httpStatusClass(301)).toBe('3xx')
  })

  it('classifies 4xx and 5xx as client/server error', () => {
    expect(httpStatusClass(404)).toBe('4xx')
    expect(httpStatusClass(500)).toBe('5xx')
  })

  it('falls back to unknown for out-of-range codes', () => {
    expect(httpStatusClass(0)).toBe('unknown')
    expect(httpStatusClass(600)).toBe('unknown')
  })
})

describe('isHttpErrorStatus', () => {
  it('does not treat 101 Switching Protocols as an error', () => {
    expect(isHttpErrorStatus(101)).toBe(false)
  })

  it('does not treat success or redirect codes as errors', () => {
    expect(isHttpErrorStatus(200)).toBe(false)
    expect(isHttpErrorStatus(301)).toBe(false)
  })

  it('treats 4xx and 5xx as errors', () => {
    expect(isHttpErrorStatus(404)).toBe(true)
    expect(isHttpErrorStatus(500)).toBe(true)
  })
})
