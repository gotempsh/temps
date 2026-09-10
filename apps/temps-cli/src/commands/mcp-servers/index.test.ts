// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, afterEach } from 'bun:test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { parseJson, resolveValue } from './index.js'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) =>
      rm(path, { recursive: true, force: true }),
    ),
  )
})

async function fixtureFile(contents: string): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-mcp-servers-test-'))
  temporaryDirectories.push(directory)
  const filePath = join(directory, 'config.json')
  await writeFile(filePath, contents)
  return filePath
}

describe('resolveValue', () => {
  test('passes an inline value through unchanged', () => {
    expect(resolveValue('{"command":"npx"}')).toBe('{"command":"npx"}')
  })

  test('reads file contents when the value is @-prefixed with a path', async () => {
    const filePath = await fixtureFile('{"command":"npx","args":["-y"]}')
    expect(resolveValue(`@${filePath}`)).toBe('{"command":"npx","args":["-y"]}')
  })

  test('wraps the read failure with the offending path, not just the raw error', () => {
    expect(() => resolveValue('@/nonexistent/mcp-config.json')).toThrow(
      /Failed to read file '\/nonexistent\/mcp-config\.json'/,
    )
  })
})

describe('parseJson', () => {
  test('parses an inline JSON string', () => {
    expect(parseJson('{"command":"docker","args":["run"]}')).toEqual({
      command: 'docker',
      args: ['run'],
    })
  })

  test('resolves @-file values before parsing, so file-based configs work too', async () => {
    const filePath = await fixtureFile('{"command":"uvx","args":["mcp-server"]}')
    expect(parseJson(`@${filePath}`)).toEqual({
      command: 'uvx',
      args: ['mcp-server'],
    })
  })

  test('rejects invalid JSON instead of sending a broken config to the server', () => {
    // A truncated/typo'd --config value must fail fast in the CLI, not
    // arrive at the API as an unparsable payload.
    expect(() => parseJson('{"command":')).toThrow(/Invalid JSON/)
  })
})
