// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test'
import { Command } from 'commander'
import { registerSandboxCommands } from './index.js'

/**
 * `sandbox exec` and `sandbox logs` against a server whose answers the test
 * controls: what reaches stdout, and how the command ends.
 */
function fakeSandboxServer() {
  const sse = (frames: string[]) =>
    new Response(frames.join(''), { headers: { 'Content-Type': 'text/event-stream' } })
  const server = Bun.serve({
    port: 0,
    hostname: '127.0.0.1',
    fetch(req) {
      const path = new URL(req.url).pathname
      if (path === '/api/v1/sandboxes/sbx_1/exec') {
        return Response.json({ exit_code: 3, stdout: 'checked 2 files\n', stderr: 'one failed\n' })
      }
      if (path === '/api/v1/sandboxes/sbx_1/jobs/job_cut/logs') {
        // The connection ends without the `done` the server sends when a job exits.
        return sse(['event: log\ndata: {"stream":"stdout","data":"step 1"}\n\n'])
      }
      if (path === '/api/v1/sandboxes/sbx_1/jobs/job_done/logs') {
        return sse([
          'event: log\ndata: {"stream":"stdout","data":"step 1"}\n\n',
          'event: done\ndata: \n\n',
        ])
      }
      return new Response('not found', { status: 404 })
    },
  })
  return { server, origin: `http://127.0.0.1:${server.port}` }
}

let fake: ReturnType<typeof fakeSandboxServer>
const saved = {
  url: process.env.TEMPS_API_URL,
  token: process.env.TEMPS_TOKEN,
  exitCode: process.exitCode,
}
let written: string[]
let spies: Array<{ mockRestore(): void }>

beforeEach(() => {
  fake = fakeSandboxServer()
  process.env.TEMPS_API_URL = fake.origin
  process.env.TEMPS_TOKEN = 'test-token'
  process.exitCode = 0
  written = []
  const capture = ((chunk: unknown) => {
    written.push(String(chunk))
    return true
  }) as typeof process.stdout.write
  spies = [
    spyOn(process.stdout, 'write').mockImplementation(capture),
    spyOn(process.stderr, 'write').mockImplementation(capture),
    spyOn(process, 'exit').mockImplementation(((code?: number) => {
      throw new Error(`process.exit(${code}) called`)
    }) as typeof process.exit),
  ]
})

afterEach(async () => {
  for (const spy of spies) spy.mockRestore()
  await fake.server.stop(true)
  process.exitCode = saved.exitCode ?? 0
  if (saved.url === undefined) delete process.env.TEMPS_API_URL
  else process.env.TEMPS_API_URL = saved.url
  if (saved.token === undefined) delete process.env.TEMPS_TOKEN
  else process.env.TEMPS_TOKEN = saved.token
})

function program(): Command {
  const root = new Command().exitOverride()
  registerSandboxCommands(root)
  return root
}

describe('sandbox exec', () => {
  test("ends with the command's exit code without cutting its output short", async () => {
    await program().parseAsync(['sandbox', 'exec', 'sbx_1', '--', 'npm', 'test'], { from: 'user' })
    expect(written).toEqual(['checked 2 files\n', 'one failed\n'])
    // Set for when the output has drained, rather than exiting at once.
    expect(process.exitCode).toBe(3)
  })
})

describe('sandbox logs', () => {
  test('follows a job to the end the server marks', async () => {
    await program().parseAsync(['sandbox', 'logs', 'sbx_1', 'job_done'], { from: 'user' })
    expect(written).toEqual(['step 1\n'])
  })

  test('a stream that closes before the job ended is not reported as finished', async () => {
    const reported: string[] = []
    const errorSpy = spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
      reported.push(args.map(String).join(' '))
    })
    try {
      // No `process.exit`: that could drop log lines still queued for a pipe.
      await program().parseAsync(['sandbox', 'logs', 'sbx_1', 'job_cut'], { from: 'user' })
    } finally {
      errorSpy.mockRestore()
    }
    expect(written).toEqual(['step 1\n'])
    expect(reported.join('\n')).toContain(
      'The log stream of job job_cut closed before the job ended, so the output above may be incomplete.',
    )
    expect(process.exitCode).toBe(1)
  })
})
