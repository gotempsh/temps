// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, beforeAll, beforeEach, describe, expect, test } from 'bun:test'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

// These run the real CLI as a subprocess because the defect lived in the seam
// between commander and the rollback implementation: the flag parsed fine and
// was dropped on the way through. A test of `rollback()` alone cannot see that.
const CLI_ENTRY = join(import.meta.dir, '../../index.ts')

let server: ReturnType<typeof Bun.serve>
let home: string
let rollbackRequests: string[] = []

beforeAll(async () => {
  home = await mkdtemp(join(tmpdir(), 'temps-rollback-test-'))
  server = Bun.serve({
    port: 0,
    hostname: '127.0.0.1',
    fetch(request) {
      const { pathname } = new URL(request.url)
      if (pathname.endsWith('/projects/by-slug/demo-app')) {
        return Response.json({ id: 7, slug: 'demo-app' })
      }
      if (pathname.endsWith('/projects/by-slug/empty-app')) {
        return Response.json({ id: 8, slug: 'empty-app' })
      }
      if (pathname.endsWith('/projects/8/deployments')) {
        return Response.json({ deployments: [], total: 0, page: 1, per_page: 20 })
      }
      if (pathname.endsWith('/projects/7/deployments')) {
        return Response.json({
          deployments: [
            {
              id: 1,
              status: 'stopped',
              created_at: 1_700_000_000,
              environment: { id: 1, name: 'production', slug: 'production', domains: [] },
            },
          ],
          total: 1,
          page: 1,
          per_page: 20,
        })
      }
      if (request.method === 'POST' && pathname.endsWith('/rollback')) {
        rollbackRequests.push(pathname)
        return Response.json({ id: 99, status: 'running' })
      }
      return Response.json({}, { status: 404 })
    },
  })
})

afterAll(async () => {
  await server.stop(true)
  await rm(home, { recursive: true, force: true })
})

beforeEach(() => {
  rollbackRequests = []
})

/** Runs the CLI the way a script does: stdin is not a terminal. */
async function runCli(...args: string[]): Promise<{ exitCode: number; output: string }> {
  const child = Bun.spawn(['bun', CLI_ENTRY, ...args], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
    cwd: home,
    env: {
      ...process.env,
      HOME: home,
      TEMPS_API_URL: `http://127.0.0.1:${server.port}`,
      TEMPS_API_TOKEN: 'test-token',
      NO_COLOR: '1',
    },
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  return { exitCode, output: stdout + stderr }
}

describe.each([
  ['temps rollback', ['rollback']],
  ['temps deployments rollback', ['deployments', 'rollback']],
])('%s without a terminal', (_name, command) => {
  test('-y rolls back without asking for confirmation', async () => {
    const { exitCode, output } = await runCli(...command, '-p', 'demo-app', '--to', '1', '-y')

    expect(output).not.toContain('Cannot ask')
    expect(exitCode).toBe(0)
    expect(rollbackRequests).toHaveLength(1)
    expect(rollbackRequests[0]).toEndWith('/projects/7/deployments/1/rollback')
  })

  test('--yes is accepted as the long form', async () => {
    const { exitCode } = await runCli(...command, '-p', 'demo-app', '--to', '1', '--yes')

    expect(exitCode).toBe(0)
    expect(rollbackRequests).toHaveLength(1)
  })

  test('without -y it still refuses rather than rolling back unconfirmed', async () => {
    const { exitCode, output } = await runCli(...command, '-p', 'demo-app', '--to', '1')

    expect(exitCode).toBe(1)
    expect(output).toContain('Rollback to deployment #1?')
    expect(rollbackRequests).toHaveLength(0)
  })

  test('-y without --to names the missing flag instead of picking a target', async () => {
    const { exitCode, output } = await runCli(...command, '-p', 'demo-app', '-y')

    expect(exitCode).toBe(1)
    expect(output).toContain('--to <deployment-id>')
    expect(rollbackRequests).toHaveLength(0)
  })

  test('without --to it fails even when there is nothing to roll back to', async () => {
    const { exitCode, output } = await runCli(...command, '-p', 'empty-app', '-y')

    expect(exitCode).toBe(1)
    expect(output).toContain('--to <deployment-id>')
    expect(rollbackRequests).toHaveLength(0)
  })
})
