// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { parseTerminalEvent, resizeMessage, terminalUrl } from './chat-terminal'

describe('terminalUrl', () => {
  test('follows the page protocol and carries the size', () => {
    expect(
      terminalUrl(
        { protocol: 'https:', host: 'console.example.test' },
        'sbx_abc',
        { cols: 120, rows: 30 }
      )
    ).toBe(
      'wss://console.example.test/api/v1/sandboxes/sbx_abc/terminal?tab=console&cols=120&rows=30'
    )
    expect(
      terminalUrl({ protocol: 'http:', host: 'localhost:3000' }, 'sbx_abc', {
        cols: 80,
        rows: 24,
      })
    ).toStartWith('ws://localhost:3000/')
  })

  test('keeps a degenerate or fractional size usable', () => {
    const url = terminalUrl({ protocol: 'http:', host: 'h' }, 'sbx_abc', {
      cols: 0,
      rows: 24.9,
    })
    expect(url).toContain('cols=2')
    expect(url).toContain('rows=24')
  })

  test('escapes an id that is not a plain slug', () => {
    expect(
      terminalUrl({ protocol: 'http:', host: 'h' }, 'a/b?c', {
        cols: 80,
        rows: 24,
      })
    ).toContain('/sandboxes/a%2Fb%3Fc/terminal')
  })
})

describe('resizeMessage', () => {
  test('is the JSON control frame the server expects', () => {
    expect(JSON.parse(resizeMessage(100, 40))).toEqual({
      type: 'resize',
      cols: 100,
      rows: 40,
    })
    expect(JSON.parse(resizeMessage(0, 1))).toEqual({
      type: 'resize',
      cols: 2,
      rows: 2,
    })
  })
})

describe('parseTerminalEvent', () => {
  test('reads each server event', () => {
    expect(
      parseTerminalEvent('{"type":"ready","tab_id":"console","existed":true}')
    ).toEqual({ type: 'ready', tabId: 'console', existed: true })
    expect(parseTerminalEvent('{"type":"exit","code":130}')).toEqual({
      type: 'exit',
      code: 130,
    })
    expect(parseTerminalEvent('{"type":"error","message":"boom"}')).toEqual({
      type: 'error',
      message: 'boom',
    })
  })

  test('ignores anything that is not a control frame', () => {
    expect(parseTerminalEvent('not json')).toBeNull()
    expect(parseTerminalEvent('null')).toBeNull()
    expect(parseTerminalEvent('{"type":"mystery"}')).toBeNull()
    expect(parseTerminalEvent('42')).toBeNull()
  })

  test('tolerates missing fields', () => {
    expect(parseTerminalEvent('{"type":"exit"}')).toEqual({
      type: 'exit',
      code: 0,
    })
    expect(parseTerminalEvent('{"type":"ready"}')).toEqual({
      type: 'ready',
      tabId: '',
      existed: false,
    })
  })
})
