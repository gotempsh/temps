// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { ipDetailPath } from './ip-detail'

test('an IPv4 address is the path as is', () => {
  expect(ipDetailPath('203.0.113.7')).toBe('/ip/203.0.113.7')
})

test('an IPv6 address stays one path segment', () => {
  expect(ipDetailPath('2001:db8::1')).toBe('/ip/2001%3Adb8%3A%3A1')
})
