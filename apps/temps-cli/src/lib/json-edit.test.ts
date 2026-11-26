// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  applyAssignments,
  getAtPath,
  parseAssignment,
  parseCliValue,
  parsePath,
  readJsonObject,
  rejectUnknownFields,
  setAtPath,
} from './json-edit.js'

const dir = mkdtempSync(join(tmpdir(), 'temps-json-edit-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))

async function* chunks(text: string) {
  yield new TextEncoder().encode(text)
}

describe('values and paths', () => {
  test('parseCliValue reads JSON when it parses and text otherwise', () => {
    expect(parseCliValue('true')).toBe(true)
    expect(parseCliValue('30')).toBe(30)
    expect(parseCliValue('["a"]')).toEqual(['a'])
    expect(parseCliValue('null')).toBeNull()
    expect(parseCliValue('https://x.example')).toBe('https://x.example')
    expect(parseCliValue('')).toBe('')
  })

  test('parseAssignment supports = (JSON or text) and := (always text)', () => {
    expect(parseAssignment('a.b=1')).toEqual({ path: ['a', 'b'], value: 1 })
    expect(parseAssignment('version:=1.10')).toEqual({ path: ['version'], value: '1.10' })
    expect(parseAssignment('url=https://x.example/?a=b')).toEqual({ path: ['url'], value: 'https://x.example/?a=b' })
    expect(() => parseAssignment('nothing')).toThrow(/not an assignment/)
    expect(() => parseAssignment('=1')).toThrow(/not an assignment/)
    expect(() => parsePath('a..b')).toThrow(/not a field path/)
  })
})

describe('setAtPath', () => {
  const doc = { a: { b: 1, c: [1] }, d: null }

  test('sets a nested field without touching the original', () => {
    const next = setAtPath(doc, ['a', 'b'], 2)
    expect(next).toEqual({ a: { b: 2, c: [1] }, d: null })
    expect(doc.a.b).toBe(1)
  })

  test('refuses unknown fields unless allowNew, listing the known ones', () => {
    expect(() => setAtPath(doc, ['a', 'x'], 1)).toThrow(/Unknown field "x". Known fields here: b, c/)
    expect(setAtPath(doc, ['a', 'x'], 1, { allowNew: true })).toEqual({ a: { b: 1, c: [1], x: 1 }, d: null })
  })

  test('creates an unset (null) section, refuses to descend into a list or value', () => {
    expect(setAtPath(doc, ['d', 'e'], true)).toEqual({ a: { b: 1, c: [1] }, d: { e: true } })
    expect(() => setAtPath(doc, ['a', 'c', 'x'], 1)).toThrow(/is a list/)
    expect(() => setAtPath(doc, ['a', 'b', 'x'], 1)).toThrow(/is a number/)
  })

  test('applyAssignments and getAtPath round-trip', () => {
    const next = applyAssignments(doc, [parseAssignment('a.b=5'), parseAssignment('d.e=x')])
    expect(getAtPath(next, ['a', 'b'])).toBe(5)
    expect(getAtPath(next, ['d', 'e'])).toBe('x')
    expect(getAtPath(next, ['missing', 'x'])).toBeUndefined()
  })
})

describe('readJsonObject and rejectUnknownFields', () => {
  test('reads a JSON object from a file or stdin', async () => {
    const file = join(dir, 'doc.json')
    writeFileSync(file, '{"a":1}')
    expect(await readJsonObject(file, 'The file')).toEqual({ a: 1 })
    expect(await readJsonObject('-', 'The file', chunks('{"b":2}'))).toEqual({ b: 2 })
  })

  test('names the source when it is missing, not JSON, or not an object', async () => {
    await expect(readJsonObject(join(dir, 'none.json'), 'The file')).rejects.toThrow(/Could not read The file/)
    await expect(readJsonObject('-', 'The file', chunks('{oops'))).rejects.toThrow(/not JSON/)
    await expect(readJsonObject('-', 'The file', chunks('[1]'))).rejects.toThrow(/must be a JSON object/)
  })

  test('rejectUnknownFields lists the unknown and the known fields', () => {
    expect(() => rejectUnknownFields({ a: 1, z: 2 }, { a: '', b: '' }, 'The body')).toThrow(/unknown field\(s\) z. Known: a, b/)
    expect(() => rejectUnknownFields({ a: 1 }, { a: '' }, 'The body')).not.toThrow()
  })
})
