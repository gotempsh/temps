// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ingressLoginAuthority,
  parseIngressLoginRequest,
} from './ingress-login'

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

  test('carries a non-default port through, for the signature and the redirect', () => {
    const request = parseIngressLoginRequest(
      '?owner_kind=sandbox&owner_id=12&scheme=http&host=ws-abc-3000.localho.st&port=8055&ts=1800000000&sig=abc'
    )
    expect(request?.port).toBe(8055)
    expect(ingressLoginAuthority(request!)).toBe('ws-abc-3000.localho.st:8055')

    const noPort = parseIngressLoginRequest(
      '?owner_kind=sandbox&owner_id=12&scheme=https&host=ws-abc-3000.example.test&ts=1800000000&sig=abc'
    )
    expect(noPort).not.toHaveProperty('port')
    expect(ingressLoginAuthority(noPort!)).toBe('ws-abc-3000.example.test')
  })

  test('refuses a malformed port or a missing id instead of guessing', () => {
    const base =
      '?owner_kind=sandbox&scheme=http&host=ws-abc-3000.localho.st&ts=1800000000&sig=abc'
    for (const port of ['0', '65536', 'x', '', '80.5', '-1']) {
      expect(
        parseIngressLoginRequest(`${base}&owner_id=12&port=${port}`)
      ).toBeNull()
    }
    // A missing owner id is not owner 0.
    expect(parseIngressLoginRequest(base)).toBeNull()
  })
})
