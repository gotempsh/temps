// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { mayBeProjectId, projectIdFromRef } from './project.js'

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

describe('mayBeProjectId', () => {
  it('reads a number as an ID only when it was typed as --project', () => {
    expect(mayBeProjectId('42', 'flag')).toBe(true)
    // A linked or configured project names a slug, which may be numeric.
    expect(mayBeProjectId('42', 'local-config')).toBe(false)
    expect(mayBeProjectId('42', 'env-var')).toBe(false)
    expect(mayBeProjectId('42', 'global-config')).toBe(false)
    expect(mayBeProjectId('my-site', 'flag')).toBe(false)
  })
})
