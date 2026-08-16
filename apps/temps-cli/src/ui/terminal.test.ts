import { describe, expect, test } from 'bun:test'
import { sanitizeTerminalText } from './terminal.js'

describe('sanitizeTerminalText', () => {
  test('strips CSI colour and cursor sequences', () => {
    expect(sanitizeTerminalText('\x1b[31mforged\x1b[0m')).toBe('forged')
    expect(sanitizeTerminalText('before\x1b[2Jafter')).toBe('beforeafter')
  })

  test('strips OSC 8 links and OSC 52 clipboard writes', () => {
    const link = '\x1b]8;;https://attacker.example\x1b\\click\x1b]8;;\x1b\\'
    const clipboard = '\x1b]52;c;dG9rX2xpdmVfc2VjcmV0\x07visible'
    expect(sanitizeTerminalText(link)).toBe('click')
    expect(sanitizeTerminalText(clipboard)).toBe('visible')
  })

  test('collapses newlines and strips C0, C1, and bidi overrides', () => {
    expect(
      sanitizeTerminalText(
        'one\r\ntwo\tthree\x00\x9b31m\u061C\u200E\u200F\u202E',
      ),
    ).toBe(
      'one two three',
    )
  })
})
