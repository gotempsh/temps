// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  INBOUND_FILTER_KINDS,
  addableInboundFilterKinds,
  inboundFilterKindInfo,
} from './inbound-filter-kinds'

describe('addableInboundFilterKinds', () => {
  test('everything is addable to a project without filters', () => {
    expect(addableInboundFilterKinds([])).toHaveLength(
      INBOUND_FILTER_KINDS.length
    )
  })

  test('a built-in kind is offered once, pattern kinds always', () => {
    const kinds = addableInboundFilterKinds([
      { kind: 'web_crawlers' },
      { kind: 'release' },
    ]).map((info) => info.kind)
    expect(kinds).not.toContain('web_crawlers')
    expect(kinds).toContain('release')
    expect(kinds).toContain('error_message')
  })
})

test('every kind is described and pattern kinds have an example', () => {
  for (const info of INBOUND_FILTER_KINDS) {
    expect(info.description.length).toBeGreaterThan(0)
    expect(info.takesPattern ? !!info.placeholder : true).toBe(true)
  }
  expect(inboundFilterKindInfo('localhost')?.label).toBe('Localhost')
  expect(inboundFilterKindInfo('nope')).toBeUndefined()
})
