// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { spawn } from 'node:child_process'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Readable } from 'node:stream'
import {
  detectLocalPackageManager,
  formatFileSize,
  installInterruptHandlers,
  killChildOnAbort,
  reconcileTimedOutImport,
  resolveTimeoutSeconds,
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
    await Bun.write(join(pnpmDir, 'pnpm-lock.yaml'), '')
    expect(detectLocalPackageManager(pnpmDir)).toBe('pnpm')

    const yarnDir = await fixtureDirectory()
    await Bun.write(join(yarnDir, 'yarn.lock'), '')
    expect(detectLocalPackageManager(yarnDir)).toBe('yarn')

    const bunDir = await fixtureDirectory()
    await Bun.write(join(bunDir, 'bun.lock'), '')
    expect(detectLocalPackageManager(bunDir)).toBe('bun')
  })

  test('prefers pnpm over yarn and npm when multiple lockfiles exist', async () => {
    // Priority order matters: a repo mid-migration between package managers
    // can have several lockfiles, and picking the wrong one generates a
    // Dockerfile that installs with the wrong tool.
    const dir = await fixtureDirectory()
    await Bun.write(join(dir, 'package-lock.json'), '')
    await Bun.write(join(dir, 'yarn.lock'), '')
    await Bun.write(join(dir, 'pnpm-lock.yaml'), '')
    expect(detectLocalPackageManager(dir)).toBe('pnpm')
  })
})

describe('uploadImageArchive', () => {
  test('times out when the upload stops making progress', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    await Bun.write(archivePath, 'image payload')

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
    await Bun.write(archivePath, 'image payload')

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
    await Bun.write(archivePath, 'payload')

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

describe('reconcileTimedOutImport', () => {
  test('resolves once the lookup finds the deployment for this exact upload attempt', async () => {
    let calls = 0
    const lookup: Parameters<typeof reconcileTimedOutImport>[0]['lookup'] = async (path) => {
      calls += 1
      expect(path.upload_request_id).toBe('upload-attempt-1')
      if (calls < 2) return { data: undefined }
      return { data: { id: 42, slug: 'myapp-7' } }
    }

    const result = await reconcileTimedOutImport({
      projectId: 1,
      environmentId: 2,
      uploadRequestId: 'upload-attempt-1',
      delayMs: 0,
      lookup,
    })

    expect(result).toEqual({ id: 42, slug: 'myapp-7' })
    expect(calls).toBe(2)
  })

  test('gives up after exhausting attempts when no deployment ever appears', async () => {
    let calls = 0
    const lookup: Parameters<typeof reconcileTimedOutImport>[0]['lookup'] = async () => {
      calls += 1
      return { data: undefined }
    }

    const result = await reconcileTimedOutImport({
      projectId: 1,
      environmentId: 2,
      uploadRequestId: 'upload-attempt-2',
      attempts: 3,
      delayMs: 0,
      lookup,
    })

    expect(result).toBeUndefined()
    expect(calls).toBe(3)
  })

  test('treats a lookup error on one attempt as a retryable miss, not a failure', async () => {
    let calls = 0
    const lookup: Parameters<typeof reconcileTimedOutImport>[0]['lookup'] = async () => {
      calls += 1
      if (calls === 1) throw new Error('network unreachable')
      return { data: { id: 9, slug: 'myapp-9' } }
    }

    const result = await reconcileTimedOutImport({
      projectId: 1,
      environmentId: 2,
      uploadRequestId: 'upload-attempt-3',
      delayMs: 0,
      lookup,
    })

    expect(result).toEqual({ id: 9, slug: 'myapp-9' })
    expect(calls).toBe(2)
  })
})

describe('withTemporaryFileCleanup', () => {
  test('removes the exported archive when upload fails', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    await Bun.write(archivePath, 'temporary image')

    await expect(
      withTemporaryFileCleanup(archivePath, async () => {
        throw new Error('upload failed')
      })
    ).rejects.toThrow('upload failed')

    expect(await Bun.file(archivePath).exists()).toBe(false)
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
    expect(await Bun.file(archivePath).text()).toBe('first-second')
  })
})

describe('resolveTimeoutSeconds', () => {
  test('defaults to 600 seconds when no flag is given', () => {
    expect(resolveTimeoutSeconds(undefined)).toBe(600)
  })

  test('accepts a positive whole number of seconds', () => {
    expect(resolveTimeoutSeconds('120')).toBe(120)
  })

  test('rejects non-numeric, fractional, zero, and negative values', () => {
    // parseInt would have silently accepted all of these — "nope" as NaN,
    // "30.5" truncated to 30, "0"/"-10" as an immediate abort.
    expect(() => resolveTimeoutSeconds('nope')).toThrow('--timeout must be a positive whole number of seconds')
    expect(() => resolveTimeoutSeconds('30.5')).toThrow('--timeout must be a positive whole number of seconds')
    expect(() => resolveTimeoutSeconds('0')).toThrow('--timeout must be a positive whole number of seconds')
    expect(() => resolveTimeoutSeconds('-10')).toThrow('--timeout must be a positive whole number of seconds')
    expect(() => resolveTimeoutSeconds('600garbage')).toThrow(
      '--timeout must be a positive whole number of seconds'
    )
  })
})

describe('installInterruptHandlers', () => {
  test('registers exactly one SIGINT and one SIGTERM listener', () => {
    const controller = new AbortController()
    const sigintBefore = process.listenerCount('SIGINT')
    const sigtermBefore = process.listenerCount('SIGTERM')

    const remove = installInterruptHandlers(controller)

    expect(process.listenerCount('SIGINT')).toBe(sigintBefore + 1)
    expect(process.listenerCount('SIGTERM')).toBe(sigtermBefore + 1)
    remove()
  })

  test('aborts the controller with the signal name when SIGINT fires', () => {
    // `process.emit` also invokes (and, since these are `.once` listeners,
    // consumes) any other SIGINT handler already registered in this process
    // — e.g. the test runner's own Ctrl-C handler — so this only asserts on
    // the controller, not on ambient listener counts.
    const controller = new AbortController()
    const remove = installInterruptHandlers(controller)

    process.emit('SIGINT')

    expect(controller.signal.aborted).toBe(true)
    expect((controller.signal.reason as Error).message).toContain('SIGINT')

    remove()
  })

  test('removes both signal listeners on cleanup without ever firing', () => {
    const controller = new AbortController()
    const sigintBefore = process.listenerCount('SIGINT')
    const sigtermBefore = process.listenerCount('SIGTERM')

    const remove = installInterruptHandlers(controller)
    remove()

    expect(process.listenerCount('SIGINT')).toBe(sigintBefore)
    expect(process.listenerCount('SIGTERM')).toBe(sigtermBefore)
    expect(controller.signal.aborted).toBe(false)
  })

  test('interruption during an operation removes its temporary archive', async () => {
    const directory = await fixtureDirectory()
    const archivePath = join(directory, 'image.tar')
    await Bun.write(archivePath, 'temporary image')

    const controller = new AbortController()
    const remove = installInterruptHandlers(controller)

    await expect(
      withTemporaryFileCleanup(archivePath, async () => {
        process.emit('SIGTERM')
        if (controller.signal.aborted) throw controller.signal.reason
      })
    ).rejects.toThrow('Local image deployment interrupted by SIGTERM')

    expect(await Bun.file(archivePath).exists()).toBe(false)
    remove()
  })
})

describe('killChildOnAbort', () => {
  test('terminates the child process once the signal aborts', async () => {
    const controller = new AbortController()
    const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'])
    const remove = killChildOnAbort(child, controller.signal)

    const exitCode = new Promise<number | null>((resolveExit) => {
      child.once('exit', resolveExit)
    })

    controller.abort()

    expect(await exitCode).not.toBe(0)
    remove()
  })

  test('kills immediately when the signal is already aborted', async () => {
    const controller = new AbortController()
    controller.abort()
    const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'])

    const exitCode = new Promise<number | null>((resolveExit) => {
      child.once('exit', resolveExit)
    })
    killChildOnAbort(child, controller.signal)

    expect(await exitCode).not.toBe(0)
  })

  test('is a no-op without a signal', () => {
    const child = spawn(process.execPath, ['-e', ''])
    expect(() => killChildOnAbort(child)).not.toThrow()
  })
})
