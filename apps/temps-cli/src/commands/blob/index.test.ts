// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { blobMetadata, blobObjectUrl, buildEnableRequest, continuationCommand, normalizePathname, registerBlobCommands, shellQuote } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const dir = mkdtempSync(join(tmpdir(), 'temps-blob-test-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))

const stored = {
  url: '/blob/7/docs/readme.txt',
  pathname: 'docs/readme.txt',
  contentType: 'text/plain',
  size: 5,
  uploadedAt: '2026-01-01T00:00:00Z',
}

describe('blob helpers', () => {
  test('normalizePathname drops leading slashes and refuses empty and .. paths', () => {
    expect(normalizePathname('/docs/a.txt')).toBe('docs/a.txt')
    expect(() => normalizePathname('  ')).toThrow(/cannot be empty/)
    expect(() => normalizePathname('docs/../../etc/passwd')).toThrow(/\.\./)
  })

  test('normalizePathname refuses "." segments, which a URL would collapse', () => {
    for (const bad of ['docs/./x', './x', 'docs/.']) expect(() => normalizePathname(bad)).toThrow(/"\." or "\.\." segments/)
    expect(normalizePathname('docs/.hidden')).toBe('docs/.hidden')
  })

  test('normalizePathname keeps whitespace, which is part of the name', () => {
    expect(normalizePathname(' a.txt ')).toBe(' a.txt ')
    expect(normalizePathname('/docs/a b.txt\t')).toBe('docs/a b.txt\t')
  })

  test('continuationCommand repeats the project, prefix and page size, shell-quoted', () => {
    expect(shellQuote('abc-1_2/x.y')).toBe('abc-1_2/x.y')
    expect(shellQuote("it's here")).toBe(`'it'\\''s here'`)
    expect(continuationCommand(7, 'my docs/', 50, 'c1')).toBe("temps blob list -p 7 --limit 50 --cursor c1 -- 'my docs/'")
    expect(continuationCommand(7, undefined, undefined, 'a b')).toBe("temps blob list -p 7 --cursor 'a b'")
    expect(continuationCommand(7, '--all', 1, 'next')).toBe('temps blob list -p 7 --limit 1 --cursor next -- --all')
  })

  test('buildEnableRequest needs the root user and password together', () => {
    expect(buildEnableRequest({})).toEqual({})
    expect(buildEnableRequest({ rootUser: 'admin', rootPassword: 'pw', image: 'rustfs/rustfs:1' })).toEqual({
      root_user: 'admin',
      root_password: 'pw',
      docker_image: 'rustfs/rustfs:1',
    })
    expect(() => buildEnableRequest({ rootUser: 'admin' })).toThrow(/together/)
    expect(() => buildEnableRequest({ rootPassword: 'pw' })).toThrow(/together/)
  })

  test('blobObjectUrl keeps slashes literal and encodes each segment', () => {
    expect(blobObjectUrl(7, 'docs/my file#1.txt')).toBe('/blob/7/docs/my%20file%231.txt')
  })

  test('blobMetadata reads the response headers', () => {
    const headers = new Headers({ 'content-type': 'image/png', 'content-length': '12', etag: '"x"' })
    expect(blobMetadata('a.png', headers)).toEqual({
      pathname: 'a.png',
      contentType: 'image/png',
      size: 12,
      lastModified: null,
      etag: '"x"',
      cacheControl: null,
    })
  })
})

describe('temps blob', () => {
  test('list --all follows cursors and passes every query field', async () => {
    const run = await runWithApi(
      registerBlobCommands,
      ['blob', 'list', 'docs/', '-p', '7', '--limit', '1', '--all', '--json'],
      {
        'GET /blob': (url: URL) =>
          url.searchParams.get('cursor') === 'next'
            ? { blobs: [{ ...stored, pathname: 'docs/b.txt' }], hasMore: false, cursor: null }
            : { blobs: [stored], hasMore: true, cursor: 'next' },
      },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([
      'GET /blob?project_id=7&prefix=docs%2F&limit=1',
      'GET /blob?project_id=7&prefix=docs%2F&limit=1&cursor=next',
    ])
    expect(JSON.parse(run.stdout).blobs).toHaveLength(2)
  })

  test('list prints a next-page command with the same project, prefix and limit', async () => {
    const run = await runWithApi(registerBlobCommands, ['blob', 'list', 'my docs/', '-p', '7', '--limit', '1'], {
      'GET /blob': { blobs: [stored], hasMore: true, cursor: 'next' },
    })
    expect(run.error).toBeUndefined()
    expect(run.stdout).toContain("temps blob list -p 7 --limit 1 --cursor next -- 'my docs/'")
  })

  test('a prefix starting with a hyphen survives the printed hint and is read back as the prefix', async () => {
    const routes = { 'GET /blob': { blobs: [stored], hasMore: true, cursor: 'next' } }
    const first = await runWithApi(registerBlobCommands, ['blob', 'list', '-p', '7', '--limit', '1', '--', '--all'], routes)
    expect(first.error).toBeUndefined()
    expect(first.requests).toEqual(['GET /blob?project_id=7&prefix=--all&limit=1'])
    const hint = first.stdout.match(/temps blob list .*/)?.[0] as string
    expect(hint).toBe('temps blob list -p 7 --limit 1 --cursor next -- --all')
    // Run the printed command: commander must take `--all` after `--` as the prefix, not as --all.
    const argv = hint.replace(/^temps /, '').split(' ')
    const second = await runWithApi(registerBlobCommands, argv, routes)
    expect(second.error).toBeUndefined()
    expect(second.requests).toEqual(['GET /blob?project_id=7&prefix=--all&limit=1&cursor=next'])
  })

  test('put uploads the file to its pathname', async () => {
    const file = join(dir, 'readme.txt')
    await Bun.write(file, 'hello')
    const run = await runWithApi(
      registerBlobCommands,
      ['blob', 'put', file, '--pathname', '/docs/readme.txt', '--content-type', 'text/plain', '--random-suffix', '-p', '7', '--json'],
      { 'POST /blob': stored },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([
      'POST /blob?project_id=7&pathname=docs%2Freadme.txt&content_type=text%2Fplain&add_random_suffix=true',
    ])
    expect(run.bodies[0]?.body).toBe('hello')
  })

  test('put of a missing file fails before calling the API', async () => {
    const run = await runWithApi(registerBlobCommands, ['blob', 'put', join(dir, 'nope.txt'), '-p', '7'], {})
    expect(run.error?.message).toMatch(/File not found/)
    expect(run.requests).toEqual([])
  })

  test('get writes the downloaded bytes to the output file', async () => {
    const output = join(dir, 'downloaded.txt')
    const run = await runWithApi(registerBlobCommands, ['blob', 'get', 'docs/readme.txt', '-p', '7', '-o', output], {
      'GET /blob/7/docs/readme.txt': new Response('hello', { headers: { 'content-type': 'text/plain' } }),
    })
    expect(run.error).toBeUndefined()
    expect(await Bun.file(output).text()).toBe('hello')
  })

  test('get reports a missing blob', async () => {
    const run = await runWithApi(registerBlobCommands, ['blob', 'get', 'gone.txt', '-p', '7', '-o', join(dir, 'x')], {
      'GET /blob/7/gone.txt': Response.json({ title: 'Not Found', detail: 'Blob gone.txt not found' }, { status: 404 }),
    })
    expect(run.error?.message).toMatch(/Could not download gone.txt/)
  })

  test('head prints metadata, and exits non-zero for a missing blob', async () => {
    const found = await runWithApi(registerBlobCommands, ['blob', 'head', 'docs/readme.txt', '-p', '7', '--json'], {
      'HEAD /blob/7/docs/readme.txt': new Response(null, { headers: { 'content-type': 'text/plain', 'content-length': '5' } }),
    })
    expect(found.error).toBeUndefined()
    expect(JSON.parse(found.stdout)).toMatchObject({ contentType: 'text/plain', size: 5 })
    const missing = await runWithApi(registerBlobCommands, ['blob', 'head', 'gone.txt', '-p', '7'], {
      'HEAD /blob/7/gone.txt': new Response(null, { status: 404 }),
    })
    expect(missing.exitCode).toBe(1)
  })

  test('head of a blob stored as text/html reads its headers instead of rejecting it as a console page', async () => {
    const run = await runWithApi(registerBlobCommands, ['blob', 'head', 'site/index.html', '-p', '7', '--json'], {
      'HEAD /blob/7/site/index.html': new Response(null, { headers: { 'content-type': 'text/html; charset=utf-8', 'content-length': '12' } }),
    })
    expect(run.error).toBeUndefined()
    expect(JSON.parse(run.stdout)).toMatchObject({ contentType: 'text/html; charset=utf-8', size: 12 })
  })

  test('get -o - keeps stdout for the bytes: the project announcement goes to stderr', async () => {
    const savedProject = process.env.TEMPS_PROJECT
    process.env.TEMPS_PROJECT = 'web'
    try {
      const run = await runWithApi(registerBlobCommands, ['blob', 'get', 'a.txt', '-o', '-'], {
        'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' },
        'GET /blob/7/a.txt': new Response(''),
      })
      expect(run.error).toBeUndefined()
      expect(run.stdout).not.toContain('Using project')
      expect(run.stderr).toContain('Using project')
    } finally {
      if (savedProject === undefined) delete process.env.TEMPS_PROJECT
      else process.env.TEMPS_PROJECT = savedProject
    }
  })

  test('copy sends source and destination', async () => {
    const run = await runWithApi(registerBlobCommands, ['blob', 'copy', 'docs/readme.txt', 'docs/copy.txt', '-p', '7'], {
      'POST /blob/copy': { ...stored, pathname: 'docs/copy.txt' },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ fromUrl: 'docs/readme.txt', toPathname: 'docs/copy.txt', projectId: 7 })
  })

  test('del refuses without a terminal and deletes with --yes', async () => {
    const refused = await runWithApi(
      registerBlobCommands,
      ['blob', 'del', 'a.txt', '-p', '7'],
      { 'DELETE /blob': { deleted: 1 } },
      { stdin: '' },
    )
    expect(refused.requests).toEqual([])
    const run = await runWithApi(registerBlobCommands, ['blob', 'del', 'a.txt', '/b.txt', '-p', '7', '--yes'], {
      'DELETE /blob': { deleted: 2 },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ pathnames: ['a.txt', 'b.txt'], projectId: 7 })
  })

  test('enable reads the root password from stdin', async () => {
    const run = await runWithApi(
      registerBlobCommands,
      ['blob', 'enable', '--root-user', 'admin', '--root-password-stdin'],
      { 'POST /blob/enable': { success: true, message: 'Blob enabled', status: {} } },
      { stdin: 'piped-password' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ root_user: 'admin', root_password: 'piped-password' })
    // The password is never echoed back.
    expect(run.stdout + run.stderr).not.toContain('piped-password')
  })

  test('status, update, disable and container-name call the service endpoints', async () => {
    const settings = {
      container_name: 'temps-blob-1',
      default_container_name: 'temps-blob-1',
      custom: false,
      in_use: true,
      required_prefix: 'temps-blob',
    }
    const routes = {
      'GET /blob/status': { enabled: true, healthy: false },
      'PATCH /blob/update': { success: true, message: 'updated', status: {} },
      'DELETE /blob/disable': { success: true, message: 'off' },
      'GET /blob/settings': settings,
      'PATCH /blob/settings': { ...settings, container_name: 'temps-blob-b', custom: true },
    }
    const status = await runWithApi(registerBlobCommands, ['blob', 'status'], routes)
    expect(status.stdout).toContain('no')
    const update = await runWithApi(registerBlobCommands, ['blob', 'update', '--image', 'rustfs/rustfs:2', '--yes'], routes)
    expect(update.bodies[0]?.body).toEqual({ docker_image: 'rustfs/rustfs:2' })
    const disable = await runWithApi(registerBlobCommands, ['blob', 'disable', '--yes'], routes)
    expect(disable.requests).toEqual(['DELETE /blob/disable'])
    const rename = await runWithApi(
      registerBlobCommands,
      ['blob', 'container-name', '--container-name', 'temps-blob-b', '--json'],
      routes,
    )
    expect(rename.bodies[0]?.body).toEqual({ container_name: 'temps-blob-b' })
    expect(JSON.parse(rename.stdout).custom).toBe(true)
  })
})
