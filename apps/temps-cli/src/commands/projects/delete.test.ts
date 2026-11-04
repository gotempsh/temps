// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, it } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const cliRoot = resolve(import.meta.dir, '../../..')
const storeModule = join(cliRoot, 'src/config/store.ts')
const cliEntry = join(cliRoot, 'src/index.ts')

/**
 * The real CLI in a child process with a throwaway home: the exit code is what
 * this file pins, and the config store writes to the user's home directory.
 */
async function run(
  args: string[],
  home: string,
  apiUrl: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const child = Bun.spawn(args, {
    cwd: cliRoot,
    env: {
      PATH: process.env.PATH,
      HOME: home,
      XDG_CONFIG_HOME: join(home, '.config'),
      TEMPS_API_URL: apiUrl,
      TEMPS_TOKEN: 'tk_test',
      NO_COLOR: '1',
    },
    stdout: 'pipe',
    stderr: 'pipe',
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  return { exitCode, stdout, stderr }
}

/** Evaluate `script` with the CLI's config store in scope, in the throwaway home. */
function withStore(script: string, home: string, apiUrl: string) {
  return run(
    [
      process.execPath,
      '-e',
      `import { config } from ${JSON.stringify(storeModule)}; ${script}`,
    ],
    home,
    apiUrl,
  )
}

describe('projects delete', () => {
  let home: string | undefined
  let server: ReturnType<typeof Bun.serve> | undefined

  afterEach(() => {
    server?.stop(true)
    server = undefined
    if (home) rmSync(home, { recursive: true, force: true })
    home = undefined
  })

  it('exits 0 and clears the default after deleting the default project', async () => {
    home = mkdtempSync(join(tmpdir(), 'temps-cli-delete-'))
    const deleted: string[] = []
    server = Bun.serve({
      port: 0,
      fetch(request) {
        const { pathname } = new URL(request.url)
        if (request.method === 'GET' && pathname.endsWith('/projects/by-slug/web')) {
          return Response.json({ id: 4, slug: 'web', name: 'web' })
        }
        if (request.method === 'DELETE' && pathname.endsWith('/projects/4')) {
          deleted.push(pathname)
          return new Response(null, { status: 204 })
        }
        return new Response('not found', { status: 404 })
      },
    })
    const apiUrl = `http://127.0.0.1:${server.port}`

    const seed = await withStore(
      `config.set('defaultProject', 'web'); console.log(config.path)`,
      home,
      apiUrl,
    )
    expect(seed.exitCode).toBe(0)
    // Guards the user's real config: the store must resolve inside the temp home.
    expect(seed.stdout.trim().startsWith(home)).toBe(true)

    const result = await run(
      [process.execPath, cliEntry, 'projects', 'delete', '--project', 'web', '--yes'],
      home,
      apiUrl,
    )

    expect(result.stderr).not.toContain('Use `delete()`')
    expect(result.exitCode).toBe(0)
    expect(deleted).toHaveLength(1)

    const after = await withStore(
      `console.log(JSON.stringify(config.get('defaultProject') ?? null))`,
      home,
      apiUrl,
    )
    expect(after.stdout.trim()).toBe('null')
  })

  it('leaves a different default project in place', async () => {
    home = mkdtempSync(join(tmpdir(), 'temps-cli-delete-'))
    server = Bun.serve({
      port: 0,
      fetch(request) {
        const { pathname } = new URL(request.url)
        if (request.method === 'GET' && pathname.endsWith('/projects/by-slug/web')) {
          return Response.json({ id: 4, slug: 'web', name: 'web' })
        }
        if (request.method === 'DELETE' && pathname.endsWith('/projects/4')) {
          return new Response(null, { status: 204 })
        }
        return new Response('not found', { status: 404 })
      },
    })
    const apiUrl = `http://127.0.0.1:${server.port}`

    await withStore(`config.set('defaultProject', 'api')`, home, apiUrl)

    const result = await run(
      [process.execPath, cliEntry, 'projects', 'delete', '--project', 'web', '--yes'],
      home,
      apiUrl,
    )
    expect(result.exitCode).toBe(0)

    const after = await withStore(
      `console.log(JSON.stringify(config.get('defaultProject') ?? null))`,
      home,
      apiUrl,
    )
    expect(after.stdout.trim()).toBe('"api"')
  })
})
