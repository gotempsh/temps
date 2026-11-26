// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expectedOffset } from './upload.js'
import { registerAiCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const dir = mkdtempSync(join(tmpdir(), 'temps-upload-test-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))

const base = '/ai/conversations/conv_1/attachment-uploads'

/** A server that holds `held` bytes of upload `up_1` and accepts only the next chunk. */
function server(held: { bytes: number }, options: { failOnce?: number } = {}) {
  let failures = options.failOnce ?? 0
  return {
    [`POST ${base}`]: { upload_id: 'up_1', chunk_bytes: 4 },
    [`PUT ${base}/up_1/chunk`]: (url: URL, body: unknown) => {
      const offset = Number(url.searchParams.get('offset'))
      if (failures > 0) {
        failures--
        return Response.json({ title: 'Unavailable', detail: 'try again' }, { status: 503 })
      }
      if (offset !== held.bytes) {
        return Response.json(
          { title: 'Chunk Out Of Order', detail: `Upload 'up_1' expected the next chunk at offset ${held.bytes} but got offset ${offset}` },
          { status: 409 },
        )
      }
      const text = String(body)
      const chunk = text.match(/\r\n\r\n([\s\S]*?)\r\n--/)?.[1] ?? ''
      held.bytes += chunk.length
      return { received_bytes: held.bytes }
    },
    [`POST ${base}/up_1/complete`]: { id: 'att_1', name: 'notes.txt', size_bytes: 10 },
  }
}

describe('temps ai chat upload', () => {
  test('expectedOffset reads the offset from the 409 detail', () => {
    expect(expectedOffset("Upload 'u' expected the next chunk at offset 96 but got offset 0")).toBe(96)
    expect(expectedOffset('something else')).toBeUndefined()
  })

  test('uploads a file in chunks of the size the server names, then completes it', async () => {
    const file = join(dir, 'notes.txt')
    await Bun.write(file, '0123456789')
    const held = { bytes: 0 }
    const run = await runWithApi(registerAiCommands, ['ai', 'chat', 'upload', 'conv_1', file, '--json'], server(held))
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([
      `POST ${base}`,
      `PUT ${base}/up_1/chunk?offset=0`,
      `PUT ${base}/up_1/chunk?offset=4`,
      `PUT ${base}/up_1/chunk?offset=8`,
      `POST ${base}/up_1/complete`,
    ])
    expect(held.bytes).toBe(10)
    expect(run.bodies[0]?.body).toEqual({ name: 'notes.txt', size_bytes: 10, mime_type: 'text/plain;charset=utf-8' })
    expect(run.stderr).toContain('--resume up_1')
    expect(JSON.parse(run.stdout).id).toBe('att_1')
  })

  test('--resume continues from the offset the server holds', async () => {
    const file = join(dir, 'notes.txt')
    await Bun.write(file, '0123456789')
    const held = { bytes: 8 }
    const run = await runWithApi(registerAiCommands, ['ai', 'chat', 'upload', 'conv_1', file, '--resume', 'up_1', '--chunk-bytes', '4'], server(held))
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([`PUT ${base}/up_1/chunk?offset=0`, `PUT ${base}/up_1/chunk?offset=8`, `POST ${base}/up_1/complete`])
    expect(held.bytes).toBe(10)
  })

  test('a transient failure is retried; a missing file fails before any request', async () => {
    const file = join(dir, 'small.txt')
    await Bun.write(file, 'abc')
    const held = { bytes: 0 }
    const run = await runWithApi(registerAiCommands, ['ai', 'chat', 'upload', 'conv_1', file], server(held, { failOnce: 1 }))
    expect(run.error).toBeUndefined()
    expect(run.requests.filter((r) => r.includes('/chunk'))).toEqual([`PUT ${base}/up_1/chunk?offset=0`, `PUT ${base}/up_1/chunk?offset=0`])
    const missing = await runWithApi(registerAiCommands, ['ai', 'chat', 'upload', 'conv_1', join(dir, 'nope.bin')], {})
    expect(missing.error?.message).toMatch(/File not found/)
    expect(missing.requests).toEqual([])
  })
})
