// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { access, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Readable } from 'node:stream'
import {
  detectLocalPackageManager,
  formatFileSize,
  uploadImageArchive,
  withTemporaryFileCleanup,
  writeArchiveStream,
} from './deploy-local-image.js'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true }))
  )
})

async function fixtureDirectory(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-local-image-test-'))
  temporaryDirectories.push(directory)
  return directory
}

describe('formatFileSize', () => {
  test('renders bytes under 1 KB as whole bytes', () => {
    expect(formatFileSize(512)).toBe('512 B')
  })

  test('renders KB and MB with one decimal', () => {
    expect(formatFileSize(2048)).toBe('2.0 KB')
    expect(formatFileSize(1024 * 1024 * 3)).toBe('3.0 MB')
  })

  test('renders GB with two decimals, unlike the deploy-static formatter', () => {
    // Docker images regularly exceed 1 GB, unlike static bundles — this
    // formatter has a GB tier the deploy-static one intentionally lacks.
    expect(formatFileSize(1024 * 1024 * 1024 * 2)).toBe('2.00 GB')
  })
})

describe('detectLocalPackageManager', () => {
  test('defaults to npm when no lockfile is present', async () => {
    const dir = await fixtureDirectory()
    expect(detectLocalPackageManager(dir)).toBe('npm')
  })

  test('detects pnpm, yarn, and bun lockfiles', async () => {
    const pnpmDir = await fixtureDirectory()
    await writeFile(join(pnpmDir, 'pnpm-lock.yaml'), '')
    expect(detectLocalPackageManager(pnpmDir)).toBe('pnpm')

    const yarnDir = await fixtureDirectory()
    await writeFile(join(yarnDir, 'yarn.lock'), '')
    expect(detectLocalPackageManager(yarnDir)).toBe('yarn')

    const bunDir = await fixtureDirectory()
    await writeFile(join(bunDir, 'bun.lock'), '')
    expect(detectLocalPackageManager(bunDir)).toBe('bun')
  })

  test('prefers pnpm over yarn and npm when multiple lockfiles exist', async () => {
    // Priority order matters: a repo mid-migration between package managers
    // can have several lockfiles, and picking the wrong one generates a
    // Dockerfile that installs with the wrong tool.
    const dir = await fixtureDirectory()
    await writeFile(join(dir, 'package-lock.json'), '')
    await writeFile(join(dir, 'yarn.lock'), '')
    await writeFile(join(dir, 'pnpm-lock.yaml'), '')
    expect(detectLocalPackageManager(dir)).toBe('pnpm')
  })
})

describe('uploadImageArchive', () => {
  test('times out when the upload stops making progress', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    await writeFile(archivePath, 'image payload')

    const fetchImpl = async (
      _input: string | URL | Request,
      init?: RequestInit
    ): Promise<Response> => {
      return await new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => reject(init.signal?.reason), {
          once: true,
        })
      })
    }

    await expect(
      uploadImageArchive({
        url: 'https://example.invalid/image-upload',
        apiKey: 'test-token',
        archivePath,
        filename: 'image.tar',
        archiveSize: 13,
        idleTimeoutMs: 20,
        responseTimeoutMs: 1_000,
        fetchImpl,
      })
    ).rejects.toThrow('Image upload made no progress for 0.02 seconds')
  })

  test('times out while the server imports an uploaded image', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    await writeFile(archivePath, 'image payload')

    let waitingForImport = false
    const fetchImpl = async (
      _input: string | URL | Request,
      init?: RequestInit
    ): Promise<Response> => {
      const body = init?.body as ReadableStream<Uint8Array>
      const reader = body.getReader()
      while (!(await reader.read()).done) {
        // Consume the full upload, then simulate an import that never responds.
      }

      return await new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => reject(init.signal?.reason), {
          once: true,
        })
      })
    }

    await expect(
      uploadImageArchive({
        url: 'https://example.invalid/image-upload',
        apiKey: 'test-token',
        archivePath,
        filename: 'image.tar',
        archiveSize: 13,
        idleTimeoutMs: 1_000,
        responseTimeoutMs: 20,
        fetchImpl,
        onAwaitingImport: () => {
          waitingForImport = true
        },
      })
    ).rejects.toThrow('server did not finish importing the image within 0.02 seconds')

    expect(waitingForImport).toBe(true)
  })

  test('sends a content length for the streamed multipart request', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    await writeFile(archivePath, 'payload')

    let requestHeaders: Headers | undefined
    let requestBody = new Uint8Array()
    const fetchImpl = async (
      _input: string | URL | Request,
      init?: RequestInit
    ): Promise<Response> => {
      requestHeaders = new Headers(init?.headers)
      requestBody = new Uint8Array(await new Response(init?.body).arrayBuffer())
      return new Response('{}', { status: 202 })
    }

    await uploadImageArchive({
      url: 'https://example.invalid/image-upload',
      apiKey: 'test-token',
      archivePath,
      filename: 'image.tar',
      archiveSize: 7,
      idleTimeoutMs: 1_000,
      responseTimeoutMs: 1_000,
      fetchImpl,
    })

    expect(Number(requestHeaders?.get('content-length'))).toBe(requestBody.byteLength)
    expect(new TextDecoder().decode(requestBody)).toContain('payload')
  })
})

describe('withTemporaryFileCleanup', () => {
  test('removes the exported archive when upload fails', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    await writeFile(archivePath, 'temporary image')

    await expect(
      withTemporaryFileCleanup(archivePath, async () => {
        throw new Error('upload failed')
      })
    ).rejects.toThrow('upload failed')

    await expect(access(archivePath)).rejects.toThrow()
  })
})

describe('writeArchiveStream', () => {
  test('resolves only after the complete archive has been flushed', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    const progress: number[] = []
    const source = Readable.from([Buffer.from('first'), Buffer.from('-second')])

    const bytesWritten = await writeArchiveStream(source, archivePath, (bytes) => {
      progress.push(bytes)
    })

    expect(bytesWritten).toBe(12)
    expect(progress).toEqual([5, 12])
    expect(await readFile(archivePath, 'utf8')).toBe('first-second')
  })
})
