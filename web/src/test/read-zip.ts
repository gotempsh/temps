// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect } from 'bun:test'
import { crc32 } from '@/lib/zip'
import { inflateRawSync } from 'node:zlib'

/** Read a stored ZIP back through its central directory, checking each CRC. */
export function readZip(zip: Uint8Array): Map<string, string> {
  const view = new DataView(zip.buffer, zip.byteOffset, zip.byteLength)
  const end = zip.length - 22
  expect(view.getUint32(end, true)).toBe(0x06054b50)
  const count = view.getUint16(end + 10, true)
  let at = view.getUint32(end + 16, true)
  const files = new Map<string, string>()
  const decoder = new TextDecoder()
  for (let i = 0; i < count; i++) {
    expect(view.getUint32(at, true)).toBe(0x02014b50)
    const crc = view.getUint32(at + 16, true)
    const size = view.getUint32(at + 24, true)
    const nameLength = view.getUint16(at + 28, true)
    const local = view.getUint32(at + 42, true)
    const name = decoder.decode(zip.subarray(at + 46, at + 46 + nameLength))
    expect(view.getUint32(local, true)).toBe(0x04034b50)
    expect(view.getUint16(local + 8, true)).toBe(0) // stored
    const start = local + 30 + view.getUint16(local + 26, true)
    const data = zip.subarray(start, start + size)
    expect(crc32(data)).toBe(crc)
    files.set(name, decoder.decode(data))
    at += 46 + nameLength
  }
  return files
}

/**
 * Read a ZIP whose entries may be deflated, checking each CRC. Returns each
 * file's text and its Unix mode.
 */
export function readDeflatedZip(
  zip: Uint8Array
): Map<string, { text: string; mode: number }> {
  const view = new DataView(zip.buffer, zip.byteOffset, zip.byteLength)
  const end = zip.length - 22
  expect(view.getUint32(end, true)).toBe(0x06054b50)
  const count = view.getUint16(end + 10, true)
  let at = view.getUint32(end + 16, true)
  const files = new Map<string, { text: string; mode: number }>()
  const decoder = new TextDecoder()
  for (let i = 0; i < count; i++) {
    expect(view.getUint32(at, true)).toBe(0x02014b50)
    const method = view.getUint16(at + 10, true)
    const crc = view.getUint32(at + 16, true)
    const compressed = view.getUint32(at + 20, true)
    const nameLength = view.getUint16(at + 28, true)
    const mode = view.getUint32(at + 38, true) >>> 16
    const local = view.getUint32(at + 42, true)
    const name = decoder.decode(zip.subarray(at + 46, at + 46 + nameLength))
    expect(view.getUint32(local, true)).toBe(0x04034b50)
    const start = local + 30 + view.getUint16(local + 26, true)
    const body = zip.subarray(start, start + compressed)
    const data = method === 8 ? inflateRawSync(body) : body
    expect(crc32(data)).toBe(crc)
    files.set(name, { text: decoder.decode(data), mode })
    at += 46 + nameLength
  }
  return files
}
