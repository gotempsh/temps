// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A minimal ZIP writer: stored (uncompressed) entries, UTF-8 names, a fixed
 * timestamp so the same input gives the same bytes. Enough to package a few
 * small text files in the browser without a dependency.
 */

export interface ZipEntry {
  /** A relative path with forward slashes, such as `server/index.js`. */
  name: string
  data: Uint8Array | string
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c >>> 0
  }
  return table
})()

export function crc32(data: Uint8Array): number {
  let crc = 0xffffffff
  for (const byte of data) crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8)
  return (crc ^ 0xffffffff) >>> 0
}

const UTF8_FLAG = 0x0800
const VERSION = 20
// 1980-01-01 00:00, the earliest DOS date.
const DOS_TIME = 0
const DOS_DATE = (1 << 5) | 1
const MAX_SIZE = 0xffffffff

function checkName(name: string): void {
  if (
    !name ||
    name.startsWith('/') ||
    name.includes('\\') ||
    name.includes('\0') ||
    name.split('/').some((part) => part === '' || part === '.' || part === '..')
  ) {
    throw new Error(`"${name}" is not a safe relative path for a ZIP entry`)
  }
}

export function createZip(entries: ZipEntry[]): Uint8Array {
  const encoder = new TextEncoder()
  const seen = new Set<string>()
  const locals: Uint8Array[] = []
  const centrals: Uint8Array[] = []
  let offset = 0

  for (const entry of entries) {
    checkName(entry.name)
    if (seen.has(entry.name)) {
      throw new Error(`"${entry.name}" appears twice in the ZIP`)
    }
    seen.add(entry.name)
    const name = encoder.encode(entry.name)
    const data =
      typeof entry.data === 'string' ? encoder.encode(entry.data) : entry.data
    const crc = crc32(data)

    const local = new Uint8Array(30 + name.length + data.length)
    const lv = new DataView(local.buffer)
    lv.setUint32(0, 0x04034b50, true)
    lv.setUint16(4, VERSION, true)
    lv.setUint16(6, UTF8_FLAG, true)
    lv.setUint16(8, 0, true) // stored
    lv.setUint16(10, DOS_TIME, true)
    lv.setUint16(12, DOS_DATE, true)
    lv.setUint32(14, crc, true)
    lv.setUint32(18, data.length, true)
    lv.setUint32(22, data.length, true)
    lv.setUint16(26, name.length, true)
    lv.setUint16(28, 0, true)
    local.set(name, 30)
    local.set(data, 30 + name.length)

    const central = new Uint8Array(46 + name.length)
    const cv = new DataView(central.buffer)
    cv.setUint32(0, 0x02014b50, true)
    cv.setUint16(4, VERSION, true)
    cv.setUint16(6, VERSION, true)
    cv.setUint16(8, UTF8_FLAG, true)
    cv.setUint16(10, 0, true)
    cv.setUint16(12, DOS_TIME, true)
    cv.setUint16(14, DOS_DATE, true)
    cv.setUint32(16, crc, true)
    cv.setUint32(20, data.length, true)
    cv.setUint32(24, data.length, true)
    cv.setUint16(28, name.length, true)
    // extra, comment, disk, internal and external attributes stay zero
    cv.setUint32(42, offset, true)
    central.set(name, 46)

    locals.push(local)
    centrals.push(central)
    offset += local.length
    if (offset > MAX_SIZE) throw new Error('The ZIP is too large without ZIP64')
  }

  const centralSize = centrals.reduce((sum, c) => sum + c.length, 0)
  const end = new Uint8Array(22)
  const ev = new DataView(end.buffer)
  ev.setUint32(0, 0x06054b50, true)
  ev.setUint16(8, entries.length, true)
  ev.setUint16(10, entries.length, true)
  ev.setUint32(12, centralSize, true)
  ev.setUint32(16, offset, true)

  const out = new Uint8Array(offset + centralSize + end.length)
  let at = 0
  for (const part of [...locals, ...centrals, end]) {
    out.set(part, at)
    at += part.length
  }
  return out
}
