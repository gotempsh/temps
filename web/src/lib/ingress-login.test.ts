// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { parseIngressLoginRequest } from './ingress-login'

describe('parseIngressLoginRequest', () => {
  test('reads every field of the signed request', () => {
    expect(
      parseIngressLoginRequest(
        '?owner_kind=environment&owner_id=7&scheme=https&host=app.example.test&next=%2Fa%3Fb%3Dc&ts=1800000000&sig=abc'
      )
    ).toEqual({
      owner_kind: 'environment',
      owner_id: 7,
      scheme: 'https',
      host: 'app.example.test',
      next: '/a?b=c',
      ts: 1800000000,
      sig: 'abc',
    })
  })

  test('refuses a request missing its signature or with a bad id', () => {
    expect(
      parseIngressLoginRequest(
        '?owner_kind=environment&owner_id=7&scheme=https&host=app.example.test&ts=1'
      )
    ).toBeNull()
    expect(
      parseIngressLoginRequest(
        '?owner_kind=environment&owner_id=x&scheme=https&host=app.example.test&ts=1&sig=a'
      )
    ).toBeNull()
    expect(parseIngressLoginRequest('')).toBeNull()
  })
})
