// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  chunkRanges,
  isRetryable,
  needsChunking,
  SINGLE_REQUEST_MAX_BYTES,
  uploadInChunks,
  UploadRequestError,
  type ChunkTransport,
} from './chunked-upload'

const noPause = () => Promise.resolve()

function fakeTransport(
  overrides: Partial<ChunkTransport<string>> = {}
): ChunkTransport<string> & { log: string[]; held: number[] } {
  const log: string[] = []
  const held: number[] = []
  return {
    log,
    held,
    start: async () => {
      log.push('start')
      return { upload_id: 'upl_1', chunk_bytes: 10 }
    },
    append: async (_id, offset, chunk) => {
      log.push(`append@${offset}+${chunk.size}`)
      held.push(offset + chunk.size)
      return offset + chunk.size
    },
    complete: async () => {
      log.push('complete')
      return 'stored'
    },
    cancel: async () => {
      log.push('cancel')
    },
    ...overrides,
  }
}

describe('chunkRanges', () => {
  test('covers the file exactly, last piece short', () => {
    expect(chunkRanges(25, 10)).toEqual([
      { start: 0, end: 10 },
      { start: 10, end: 20 },
      { start: 20, end: 25 },
    ])
  })

  test('a file that fits is one piece; an empty one has none', () => {
    expect(chunkRanges(7, 10)).toEqual([{ start: 0, end: 7 }])
    expect(chunkRanges(0, 10)).toEqual([])
  })

  test('refuses a chunk size that cannot make progress', () => {
    expect(() => chunkRanges(5, 0)).toThrow(RangeError)
  })
})

describe('needsChunking', () => {
  test('small files go in one request, bigger ones in chunks', () => {
    expect(needsChunking(SINGLE_REQUEST_MAX_BYTES)).toBe(false)
    expect(needsChunking(SINGLE_REQUEST_MAX_BYTES + 1)).toBe(true)
  })

  test('the single-request ceiling leaves room under a 40 KiB proxy cap', () => {
    expect(SINGLE_REQUEST_MAX_BYTES + 4 * 1024).toBeLessThan(40 * 1024)
  })
})

describe('uploadInChunks', () => {
  test('sends the pieces in order and returns the completion', async () => {
    const transport = fakeTransport()
    const file = new File([new Uint8Array(25)], 'shot.png', {
      type: 'image/png',
    })
    const progress: number[] = []
    const result = await uploadInChunks(file, transport, {
      pause: noPause,
      onProgress: (received) => progress.push(received),
    })
    expect(result).toBe('stored')
    expect(transport.log).toEqual([
      'start',
      'append@0+10',
      'append@10+10',
      'append@20+5',
      'complete',
    ])
    expect(progress).toEqual([10, 20, 25])
  })

  test('retries a chunk after a network failure and carries on', async () => {
    let failures = 1
    const transport = fakeTransport({
      append: async (_id, offset, chunk) => {
        if (offset === 10 && failures-- > 0) throw new TypeError('offline')
        return offset + chunk.size
      },
    })
    const file = new File([new Uint8Array(25)], 'a.bin')
    expect(await uploadInChunks(file, transport, { pause: noPause })).toBe(
      'stored'
    )
  })

  test('does not retry a rejection, and cancels so the server frees it', async () => {
    const transport = fakeTransport({
      append: async () => {
        throw new UploadRequestError('chunk out of order', 409)
      },
    })
    const file = new File([new Uint8Array(25)], 'a.bin')
    await expect(
      uploadInChunks(file, transport, { pause: noPause })
    ).rejects.toThrow('chunk out of order')
    expect(transport.log).toEqual(['start', 'cancel'])
  })

  test('gives up after the retries and cancels', async () => {
    let calls = 0
    const transport = fakeTransport({
      append: async () => {
        calls += 1
        throw new UploadRequestError('bad gateway', 502)
      },
    })
    const file = new File([new Uint8Array(5)], 'a.bin')
    await expect(
      uploadInChunks(file, transport, { pause: noPause, retries: 2 })
    ).rejects.toThrow('bad gateway')
    expect(calls).toBe(3)
    expect(transport.log[transport.log.length - 1]).toBe('cancel')
  })

  test('a failing completion cancels and rethrows', async () => {
    const transport = fakeTransport({
      complete: async () => {
        throw new UploadRequestError('workspace quota exceeded', 400)
      },
    })
    const file = new File([new Uint8Array(5)], 'a.bin')
    await expect(
      uploadInChunks(file, transport, { pause: noPause })
    ).rejects.toThrow('workspace quota exceeded')
    expect(transport.log[transport.log.length - 1]).toBe('cancel')
  })

  test('a failing cancel does not hide the original error', async () => {
    const transport = fakeTransport({
      complete: async () => {
        throw new UploadRequestError('nope', 400)
      },
      cancel: async () => {
        throw new Error('cancel failed')
      },
    })
    const file = new File([new Uint8Array(5)], 'a.bin')
    await expect(
      uploadInChunks(file, transport, { pause: noPause })
    ).rejects.toThrow('nope')
  })
})

describe('isRetryable', () => {
  test('retries transport failures and server errors only', () => {
    expect(isRetryable(new TypeError('failed to fetch'))).toBe(true)
    expect(isRetryable(new UploadRequestError('x'))).toBe(true)
    expect(isRetryable(new UploadRequestError('x', 503))).toBe(true)
    expect(isRetryable(new UploadRequestError('x', 404))).toBe(false)
    expect(isRetryable(new UploadRequestError('x', 413))).toBe(false)
    expect(isRetryable(new Error('other'))).toBe(false)
  })
})
