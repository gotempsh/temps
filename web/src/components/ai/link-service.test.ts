// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { linkCandidates } from './link-service'

const projects = [{ id: 1 }, { id: 2 }, { id: 3 }, { id: 4 }]

describe('linkCandidates', () => {
  test('offers the projects this chat made first', () => {
    expect(linkCandidates(projects, [], [3])).toEqual({
      chat: [{ id: 3 }],
      other: [{ id: 1 }, { id: 2 }, { id: 4 }],
    })
  })

  test('leaves out projects already linked, wherever they were made', () => {
    expect(linkCandidates(projects, [3, 1], [3])).toEqual({
      chat: [],
      other: [{ id: 2 }, { id: 4 }],
    })
  })

  test('a chat project the user cannot see is not offered', () => {
    expect(linkCandidates([{ id: 1 }], [], [9]).chat).toEqual([])
  })
})
