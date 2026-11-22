// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  canEmbed,
  normalizePreviewPath,
  parsePort,
  parsePreviewTarget,
  previewAddress,
  previewUrlFor,
} from './chat-preview'

describe('parsePort', () => {
  test('accepts whole port numbers, ignoring surrounding space', () => {
    expect(parsePort('3000')).toBe(3000)
    expect(parsePort(' 8080 ')).toBe(8080)
    expect(parsePort('65535')).toBe(65535)
    expect(parsePort('1')).toBe(1)
  })

  test('rejects anything that is not a port', () => {
    for (const text of [
      '',
      '0',
      '65536',
      '-1',
      '30.5',
      'abc',
      '3000a',
      '1e3',
    ]) {
      expect(parsePort(text)).toBeNull()
    }
  })
})

describe('previewUrlFor', () => {
  test('puts the port into the template', () => {
    expect(previewUrlFor('http://ws-abc-{port}.localho.st:8080', 5173)).toBe(
      'http://ws-abc-5173.localho.st:8080'
    )
  })
})

describe('canEmbed', () => {
  test('refuses an http page inside an https console only', () => {
    expect(canEmbed('http://ws-a-1.localho.st:8080', 'https:')).toBe(false)
    expect(canEmbed('https://ws-a-1.example.test', 'https:')).toBe(true)
    expect(canEmbed('http://ws-a-1.localho.st:8080', 'http:')).toBe(true)
  })
})

const TEMPLATE = 'http://ws-abc-{port}.localho.st:8080'

describe('normalizePreviewPath', () => {
  test('adds the leading slash and keeps query and fragment', () => {
    expect(normalizePreviewPath('')).toBe('/')
    expect(normalizePreviewPath('pricing')).toBe('/pricing')
    expect(normalizePreviewPath('/a/b?x=1&y=2#top')).toBe('/a/b?x=1&y=2#top')
  })

  test('refuses anything that would leave the preview host', () => {
    expect(normalizePreviewPath('//evil.example')).toBeNull()
    expect(normalizePreviewPath('/\\evil.example')).toBeNull()
  })

  test('refuses whitespace and control characters inside the path', () => {
    expect(normalizePreviewPath('/a b')).toBeNull()
    expect(normalizePreviewPath('/a\nb')).toBeNull()
  })
})

describe('parsePreviewTarget', () => {
  test('a path stays on the current port', () => {
    expect(parsePreviewTarget('/pricing', 3000, TEMPLATE)).toEqual({
      port: 3000,
      path: '/pricing',
    })
    expect(parsePreviewTarget('pricing?plan=pro', 3000, TEMPLATE)).toEqual({
      port: 3000,
      path: '/pricing?plan=pro',
    })
  })

  test('a bare port or a port with a path switches port', () => {
    expect(parsePreviewTarget('5173', 3000, TEMPLATE)).toEqual({
      port: 5173,
      path: '/',
    })
    expect(parsePreviewTarget(':5173/about', 3000, TEMPLATE)).toEqual({
      port: 5173,
      path: '/about',
    })
    expect(parsePreviewTarget(':5173', null, TEMPLATE)).toEqual({
      port: 5173,
      path: '/',
    })
  })

  test('a path needs a port to be on', () => {
    expect(parsePreviewTarget('/pricing', null, TEMPLATE)).toBeNull()
  })

  test('a full address on the preview host is read back into port and path', () => {
    expect(
      parsePreviewTarget(
        'http://ws-abc-4000.localho.st:8080/docs?x=1',
        3000,
        TEMPLATE
      )
    ).toEqual({ port: 4000, path: '/docs?x=1' })
  })

  test('an address on any other host is refused', () => {
    for (const text of [
      'https://example.com/',
      'http://ws-other-3000.localho.st:8080/',
      'http://ws-abc-3000.localho.st:9999/',
      'http://ws-abc-3000.localho.st.evil.example:8080/',
      'http://ws-abc-3000x.localho.st:8080/',
    ]) {
      expect(parsePreviewTarget(text, 3000, TEMPLATE)).toBeNull()
    }
  })

  test('works with a template that carries the port in the port position', () => {
    const template = 'http://localhost:{port}'
    expect(
      parsePreviewTarget('http://localhost:5173/x', null, template)
    ).toEqual({ port: 5173, path: '/x' })
    expect(
      parsePreviewTarget('http://localhost.evil.example:5173/', null, template)
    ).toBeNull()
  })

  test('an out-of-range port is refused', () => {
    expect(parsePreviewTarget(':70000/x', 3000, TEMPLATE)).toBeNull()
  })
})

describe('previewAddress', () => {
  test('joins the preview origin and the path', () => {
    expect(previewAddress(TEMPLATE, { port: 3000, path: '/pricing?a=1' })).toBe(
      'http://ws-abc-3000.localho.st:8080/pricing?a=1'
    )
  })

  test('round-trips through the parser', () => {
    const target = { port: 4321, path: '/a/b?c=d#e' }
    expect(
      parsePreviewTarget(previewAddress(TEMPLATE, target), null, TEMPLATE)
    ).toEqual(target)
  })
})
