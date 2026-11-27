// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readZip } from '@/test/read-zip'
import { crc32, createZip } from './zip'

describe('zip', () => {
  test('crc32 matches the standard check value', () => {
    expect(crc32(new TextEncoder().encode('123456789'))).toBe(0xcbf43926)
    expect(crc32(new Uint8Array())).toBe(0)
  })

  test('entries read back unchanged, UTF-8 names included', () => {
    const zip = createZip([
      { name: 'manifest.json', data: '{"name":"temps"}\n' },
      { name: 'server/index.js', data: 'console.log("é")\n' },
      { name: 'raw.bin', data: new Uint8Array([0, 255, 10]) },
    ])
    const files = readZip(zip)
    expect([...files.keys()]).toEqual([
      'manifest.json',
      'server/index.js',
      'raw.bin',
    ])
    expect(files.get('server/index.js')).toBe('console.log("é")\n')
  })

  test('the same input gives the same bytes', () => {
    const entries = [{ name: 'a.txt', data: 'a' }]
    expect(createZip(entries)).toEqual(createZip(entries))
  })

  test('unsafe or duplicate names are refused', () => {
    for (const name of [
      '',
      '/etc/passwd',
      '../escape',
      'a/../../b',
      'a\\b',
      'a//b',
      './a',
    ]) {
      expect(() => createZip([{ name, data: 'x' }])).toThrow(
        'not a safe relative path'
      )
    }
    expect(() =>
      createZip([
        { name: 'a', data: '1' },
        { name: 'a', data: '2' },
      ])
    ).toThrow('appears twice')
  })
})
