// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  accessTokenFormSchema,
  deleteBlockedReason,
  isAccountId,
  keysLabel,
  protectsLabel,
} from './cloudflare-access'

const account = '0123456789abcdef0123456789abcdef'

describe('Cloudflare Access token form', () => {
  it('takes a 32-character hexadecimal account id only', () => {
    expect(isAccountId(account)).toBe(true)
    expect(isAccountId(` ${account.toUpperCase()} `)).toBe(true)
    expect(isAccountId(account.slice(1))).toBe(false)
    expect(isAccountId(`${account.slice(1)}g`)).toBe(false)
  })

  it('needs a name, an account id and a whole token', () => {
    expect(
      accessTokenFormSchema.safeParse({
        name: 'Access',
        account_id: account,
        token: 'x'.repeat(40),
      }).success
    ).toBe(true)
    const refused = accessTokenFormSchema.safeParse({
      name: ' ',
      account_id: 'nope',
      token: 'short',
    })
    expect(refused.success).toBe(false)
    if (!refused.success)
      expect(refused.error.issues.map((i) => i.path[0]).sort()).toEqual([
        'account_id',
        'name',
        'token',
      ])
  })
})

describe('saved Access tokens', () => {
  it('says how many addresses a token protects', () => {
    expect(protectsLabel(0)).toBe('Protects no address yet')
    expect(protectsLabel(1)).toBe('Protects 1 address')
    expect(protectsLabel(3)).toBe('Protects 3 addresses')
  })

  it('refuses to delete a token in use, and says what to do', () => {
    expect(deleteBlockedReason(0)).toBeNull()
    expect(deleteBlockedReason(2)).toContain('Temps sign-in')
  })

  it('says when the signing keys were never fetched', () => {
    expect(keysLabel(0, null)).toContain('every hour')
    expect(keysLabel(2, '2026-10-26T10:00:00Z')).toBe('2 signing keys')
  })
})
