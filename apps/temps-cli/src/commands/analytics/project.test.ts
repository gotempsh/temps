// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { projectIdFromRef } from './project.js'

describe('projectIdFromRef', () => {
  it('reads a positive integer as a project ID', () => {
    expect(projectIdFromRef('42')).toBe(42)
    expect(projectIdFromRef('2147483647')).toBe(2147483647)
  })

  it('treats anything else as a slug', () => {
    for (const ref of ['my-site', '0', '007', '-3', '4.5', '42abc', '', '2147483648', '99999999999']) {
      expect(projectIdFromRef(ref)).toBeUndefined()
    }
  })
})
