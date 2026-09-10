// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { resolveValue } from './secrets.js'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

async function fixtureDirectory(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-secrets-test-'))
  temporaryDirectories.push(directory)
  return directory
}

describe('resolveValue', () => {
  test('passes a plain value through untouched', () => {
    expect(resolveValue('plaintext-secret')).toBe('plaintext-secret')
  })

  test('reads the value from a file when prefixed with @', async () => {
    const directory = await fixtureDirectory()
    const filePath = join(directory, 'auth.json')
    await writeFile(filePath, '{"token":"abc"}')

    expect(resolveValue(`@${filePath}`)).toBe('{"token":"abc"}')
  })

  test('raises a readable error when the @-prefixed file does not exist', () => {
    // Without this, a typo'd path would surface as an opaque fs stack trace
    // instead of naming the file the CLI actually tried to read.
    expect(() => resolveValue('@/no/such/file.json')).toThrow('/no/such/file.json')
  })
})
