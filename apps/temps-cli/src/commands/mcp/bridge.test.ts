// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { bridgeApiKey, bridgePackageSpec, checkBridgeAddress, resolveFolder } from './bridge.js'

const CLI_ENTRY = fileURLToPath(new URL('../../index.ts', import.meta.url))
const KEY = 'tk_test_secret_value_0123456789'
const SCRATCH = realpathSync(mkdtempSync(join(tmpdir(), 'temps-cli-bridge-')))
afterAll(() => rmSync(SCRATCH, { recursive: true, force: true }))

describe('bridge settings', () => {
  test('folders become full paths', () => {
    expect(resolveFolder('~/code')).toBe(join(homedir(), 'code'))
    expect(resolveFolder('~')).toBe(homedir())
    expect(resolveFolder(' apps ')).toBe(resolve('apps'))
    expect(resolveFolder('/srv/apps/')).toBe('/srv/apps')
  })

  test('a blank folder is refused, never read as the current one', () => {
    for (const blank of ['', '   ', '\t']) {
      expect(() => resolveFolder(blank)).toThrow('left blank')
    }
  })

  test('the package is pinned to this CLI', () => {
    expect(bridgePackageSpec('1.2.3')).toBe('@temps-sdk/cli@1.2.3')
  })

  test('an address the bridge would refuse is refused up front', () => {
    expect(() => checkBridgeAddress('https://temps.example.test')).not.toThrow()
    expect(() => checkBridgeAddress('http://localhost:3014')).not.toThrow()
    expect(() => checkBridgeAddress('http://192.168.1.10:8080')).toThrow('plain HTTP')
  })

  test('the key comes from the file TEMPS_API_KEY_FILE names, or else from TEMPS_API_KEY', () => {
    const file = join(SCRATCH, 'bridge.key')
    writeFileSync(file, `${KEY}\n`, { mode: 0o600 })
    expect(bridgeApiKey({ TEMPS_API_KEY_FILE: file })).toBe(KEY)
    // The entry's file is for the entry's --url; a key inherited from the
    // app's environment may be another instance's, and is never sent.
    expect(bridgeApiKey({ TEMPS_API_KEY: 'inherited', TEMPS_API_KEY_FILE: file })).toBe(KEY)
    expect(bridgeApiKey({ TEMPS_API_KEY: 'direct' })).toBe('direct')
    expect(bridgeApiKey({})).toBe('')
  })

  test('a key file that is missing or too large is an error naming the file, not a read into memory', () => {
    const missing = join(SCRATCH, 'missing.key')
    expect(() => bridgeApiKey({ TEMPS_API_KEY_FILE: missing })).toThrow(missing)
    const large = join(SCRATCH, 'large.key')
    writeFileSync(large, 'x'.repeat(5000))
    expect(() => bridgeApiKey({ TEMPS_API_KEY_FILE: large })).toThrow('not an API key file')
  })
})

describe('temps mcp bridge', () => {
  test('speaks MCP on stdout only, forwards to Temps with the key, and deploys the allowed folder in one call', async () => {
    const project = join(SCRATCH, 'projects', 'my-app')
    mkdirSync(project, { recursive: true })
    writeFileSync(join(project, 'index.html'), '<h1>hi</h1>')
    writeFileSync(join(project, '.env'), 'TOKEN=secret')

    const seen: { path: string; auth: string | null }[] = []
    let uploaded: File | null = null
    const server = Bun.serve({
      hostname: '127.0.0.1',
      port: 0,
      async fetch(req) {
        const url = new URL(req.url)
        seen.push({ path: `${url.pathname}${url.search}`, auth: req.headers.get('authorization') })
        if (url.pathname === '/mcp/tools') {
          return Response.json({ groups: [] })
        }
        if (url.pathname === '/mcp') {
          const body = await req.json()
          const result = body.method === 'tools/list' ? { tools: [{ name: 'temps' }] } : { ok: true }
          return Response.json({ jsonrpc: '2.0', id: body.id, result })
        }
        if (url.pathname === '/api/projects/4/environments') {
          return Response.json([{ id: 7, name: 'production', slug: 'production', is_preview: false }])
        }
        if (url.pathname === '/api/projects/4/environments/7/deploy/source') {
          uploaded = (await req.formData()).get('file') as File
          return Response.json({ id: 42 }, { status: 202 })
        }
        return new Response('', { status: 404 })
      },
    })

    const home = join(SCRATCH, 'home')
    mkdirSync(home, { recursive: true })
    const child = Bun.spawn(
      [
        process.execPath,
        CLI_ENTRY,
        'mcp',
        'bridge',
        '--url',
        `http://127.0.0.1:${server.port}`,
        '--folder',
        join(SCRATCH, 'projects'),
      ],
      {
        env: { PATH: process.env.PATH ?? '', HOME: home, TEMPS_API_KEY: KEY },
        stdin: 'pipe',
        stdout: 'pipe',
        stderr: 'pipe',
      },
    )
    const deploy = (id: number, extra: Record<string, string> = {}) => ({
      jsonrpc: '2.0',
      id,
      method: 'tools/call',
      params: { name: 'temps_deploy_folder', arguments: { path: project, project_id: 4, ...extra } },
    })
    const messages: ((lines: string[]) => { id: number })[] = [
      () => ({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }),
      () => ({ jsonrpc: '2.0', id: 2, method: 'tools/list' }),
      () => deploy(3),
    ]
    const lines: string[] = []
    const reader = child.stdout.getReader()
    const decoder = new TextDecoder()
    let buffer = ''
    try {
      for (const next of messages) {
        const message = next(lines)
        child.stdin.write(`${JSON.stringify(message)}\n`)
        child.stdin.flush()
        const deadline = Date.now() + 15_000
        while (lines.length < message.id && Date.now() < deadline) {
          const { value, done } = await reader.read()
          if (done) break
          buffer += decoder.decode(value)
          let newline
          while ((newline = buffer.indexOf('\n')) !== -1) {
            lines.push(buffer.slice(0, newline))
            buffer = buffer.slice(newline + 1)
          }
        }
      }
    } finally {
      child.stdin.end()
      child.kill()
      server.stop(true)
    }
    const stderr = await new Response(child.stderr).text()

    // Nothing but MCP messages on stdout.
    const replies = lines.map((line) => JSON.parse(line))
    expect(replies.map((r) => r.id)).toEqual([1, 2, 3])
    expect(replies[1].result.tools.map((t: { name: string }) => t.name)).toEqual(['temps', 'temps_deploy_folder'])
    expect(replies[2].result.isError).toBe(false)
    expect(replies[2].result.content[0].text).toContain('Deployment 42 of project 4 (production) started')
    expect(replies[2].result.content[0].text).toContain('Left out: .env.')
    expect(uploaded).not.toBeNull()

    expect(seen.map((s) => s.path)).toEqual([
      '/mcp?write=1',
      '/mcp?write=1',
      '/mcp/tools',
      '/api/projects/4/environments',
      '/api/projects/4/environments/7/deploy/source',
    ])
    expect(seen.every((s) => s.auth === `Bearer ${KEY}`)).toBe(true)
    expect(stderr).toContain(`temps: may deploy from ${join(SCRATCH, 'projects')}`)
    expect(stderr).not.toContain(KEY)
  }, 30_000)

  test('with no folder allowed, it says to run temps mcp add, not to open Claude', async () => {
    const child = Bun.spawn([process.execPath, CLI_ENTRY, 'mcp', 'bridge', '--url', 'https://temps.example.test'], {
      env: { PATH: process.env.PATH ?? '', HOME: join(SCRATCH, 'home'), TEMPS_API_KEY: KEY },
      stdin: 'pipe',
      stdout: 'pipe',
      stderr: 'pipe',
    })
    child.stdin.write(
      `${JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'tools/call',
        params: { name: 'temps_deploy_folder', arguments: { path: SCRATCH, project_id: 4 } },
      })}\n`,
    )
    child.stdin.end()
    const out = await new Response(child.stdout).text()
    const reply = JSON.parse(out.trim().split('\n')[0] ?? '{}')
    expect(reply.result.isError).toBe(true)
    expect(reply.result.content[0].text).toContain('temps mcp add <app> --deploy-folder')
    expect(reply.result.content[0].text).not.toContain('Claude')
  }, 30_000)
})
