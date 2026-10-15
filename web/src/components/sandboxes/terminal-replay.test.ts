// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  appendOutputTail,
  MIN_REPLAY_ANCHOR_BYTES,
  REPLAY_ANCHOR_BYTES,
  resyncReplay,
} from './terminal-replay'

const encoder = new TextEncoder()
const decoder = new TextDecoder()
const bytes = (text: string) => encoder.encode(text)
const text = (value: Uint8Array) => decoder.decode(value)

// Numbered lines make every window of the stream unique.
function stream(from: number, to: number): string {
  let out = ''
  for (let line = from; line < to; line += 1) {
    out += `build step ${String(line).padStart(5, '0')} finished ok\r\n`
  }
  return out
}

describe('appendOutputTail', () => {
  test('keeps only the last bytes of a long write', () => {
    const tail = appendOutputTail(new Uint8Array(0), bytes('x'.repeat(1000)))
    expect(tail.length).toBe(REPLAY_ANCHOR_BYTES)
  })

  test('slides the window across writes', () => {
    const first = appendOutputTail(new Uint8Array(0), bytes('abcdef'), 8)
    const second = appendOutputTail(first, bytes('ghijkl'), 8)
    expect(text(second)).toBe('efghijkl')
  })

  test('keeps a short history whole', () => {
    expect(text(appendOutputTail(bytes('ab'), bytes('cd'), 8))).toBe('abcd')
  })
})

describe('resyncReplay', () => {
  test('writes only what followed the output the terminal already has', () => {
    const before = stream(0, 600)
    const gap = stream(600, 640)
    const seen = appendOutputTail(new Uint8Array(0), bytes(before))
    // The replay window covers the tail of the old output plus the gap.
    const replay = bytes(before.slice(-4000) + gap)

    const result = resyncReplay(seen, replay)

    expect(result.reset).toBe(false)
    expect(text(result.bytes)).toBe(gap)
  })

  test('writes nothing new when the shell was quiet', () => {
    const before = stream(0, 300)
    const seen = appendOutputTail(new Uint8Array(0), bytes(before))

    const result = resyncReplay(seen, bytes(before.slice(-4000)))

    expect(result.reset).toBe(false)
    expect(result.bytes.length).toBe(0)
  })

  test('starts over when the gap outran the replay window', () => {
    const seen = appendOutputTail(new Uint8Array(0), bytes(stream(0, 300)))
    const replay = bytes(stream(5000, 5100))

    const result = resyncReplay(seen, replay)

    expect(result.reset).toBe(true)
    expect(text(result.bytes)).toBe(stream(5000, 5100))
  })

  test('starts over when there is too little history to anchor on', () => {
    const seen = bytes('$ ')
    expect(seen.length).toBeLessThan(MIN_REPLAY_ANCHOR_BYTES)
    const replay = bytes('$ ls\r\nfile\r\n$ ')

    const result = resyncReplay(seen, replay)

    expect(result.reset).toBe(true)
    expect(text(result.bytes)).toBe('$ ls\r\nfile\r\n$ ')
  })

  test('starts over on the first attach, before anything was seen', () => {
    const result = resyncReplay(new Uint8Array(0), bytes(stream(0, 5)))

    expect(result.reset).toBe(true)
    expect(text(result.bytes)).toBe(stream(0, 5))
  })

  test('never drops output when the stream repeats itself', () => {
    const line = '.'.repeat(80) + '\r\n'
    const before = line.repeat(10)
    const gap = line.repeat(3) + 'done\r\n'
    const seen = appendOutputTail(new Uint8Array(0), bytes(before))

    const result = resyncReplay(seen, bytes(before + gap))

    expect(result.reset).toBe(false)
    // The anchor matches the earliest place it can, so some repeated lines may
    // show again, but everything produced while disconnected is present.
    expect(text(result.bytes).endsWith(gap)).toBe(true)
  })
})
