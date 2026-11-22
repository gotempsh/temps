// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { nextCapabilities, normalizeCapability } from './container-security.js'

describe('normalizeCapability', () => {
  test('accepts the Docker, kernel and setcap spellings', () => {
    expect(normalizeCapability('NET_BIND_SERVICE')).toBe('NET_BIND_SERVICE')
    expect(normalizeCapability('cap_net_bind_service')).toBe('NET_BIND_SERVICE')
    expect(normalizeCapability(' CAP_NET_BIND_SERVICE ')).toBe('NET_BIND_SERVICE')
  })

  test('rejects anything that is not a capability name', () => {
    expect(() => normalizeCapability('NET BIND')).toThrow(/not a Linux capability/)
    expect(() => normalizeCapability('')).toThrow(/not a Linux capability/)
    expect(() => normalizeCapability('--privileged')).toThrow(/not a Linux capability/)
  })
})

describe('nextCapabilities', () => {
  test('adds and removes, sorted and without duplicates', () => {
    expect(nextCapabilities([], ['NET_BIND_SERVICE'], [])).toEqual(['NET_BIND_SERVICE'])
    expect(nextCapabilities(['NET_BIND_SERVICE'], ['NET_BIND_SERVICE'], [])).toEqual(['NET_BIND_SERVICE'])
    expect(nextCapabilities(['NET_BIND_SERVICE'], [], ['NET_BIND_SERVICE'])).toEqual([])
  })

  test('refuses to allow and remove the same capability', () => {
    expect(() => nextCapabilities([], ['NET_BIND_SERVICE'], ['NET_BIND_SERVICE'])).toThrow(/both allow and remove/)
  })
})
