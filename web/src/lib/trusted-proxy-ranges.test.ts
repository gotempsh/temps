// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { parseProxyRanges, validateProxyRanges } from './trusted-proxy-ranges'

test('accepts empty, IPv4, IPv6 and mapped proxy ranges', () => {
  for (const value of [
    '',
    '10.20.0.2\n2001:db8::/48',
    '::ffff:10.20.0.2/128',
    '10.20.0.0/16, ::1',
  ]) {
    expect(validateProxyRanges(value)).toBe(true)
  }
  expect(parseProxyRanges(' 10.20.0.2,\n ::1 \n')).toEqual(['10.20.0.2', '::1'])
})
test('rejects invalid syntax, bounds and unrestricted ranges before Save', () => {
  for (const value of [
    'nope',
    '300.1.2.3',
    '01.2.3.4',
    '10.0.0.1:80',
    '10.0.0.1/33',
    '::1/129',
    '::/0',
    '0.0.0.0/0',
    '::ffff:0.0.0.0/96',
    '1::2::3',
    '[::1]',
    '::1%lo',
    '::1/',
    '::1/64/1',
    Array(65).fill('::1').join('\n'),
  ]) {
    expect(validateProxyRanges(value)).toBeString()
  }
})
