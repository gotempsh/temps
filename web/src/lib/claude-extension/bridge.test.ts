// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, afterEach, describe, expect, test } from 'bun:test'
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { readDeflatedZip } from '@/test/read-zip'
import { CLAUDE_EXTENSION_ENTRY } from './extension'

const BRIDGE_PATH = fileURLToPath(new URL('./bridge.cjs', import.meta.url))
const DEPLOY_PATH = fileURLToPath(new URL('./deploy.cjs', import.meta.url))

// The extension as Claude unpacks it: the entry file next to the server.
const EXTENSION_DIR = mkdtempSync(join(tmpdir(), 'temps-extension-'))
const ENTRY_PATH = join(EXTENSION_DIR, 'index.js')
writeFileSync(ENTRY_PATH, CLAUDE_EXTENSION_ENTRY)
copyFileSync(BRIDGE_PATH, join(EXTENSION_DIR, 'bridge.cjs'))
copyFileSync(DEPLOY_PATH, join(EXTENSION_DIR, 'deploy.cjs'))
// A host that loads the entry through a wrapper of its own, so the entry is
// not `require.main`, the way the Claude app's built-in Node runs it.
const WRAPPED_PATH = join(EXTENSION_DIR, 'host-wrapper.js')
writeFileSync(WRAPPED_PATH, `require(${JSON.stringify(ENTRY_PATH)})\n`)
afterAll(() => rmSync(EXTENSION_DIR, { recursive: true, force: true }))

interface FakeSocket extends EventEmitter {
  localAddress?: string
  destroy(): void
}

interface Bridge {
  plainHttpAllowed(address: string): boolean
  tailnetLocalAddresses(
    interfaces: Record<string, { address: string }[]>
  ): Set<string>
  plainHttpSource(
    origin: string,
    address: string,
    port: number,
    local: Set<string>,
    connect: (options: { host: string; port: number }) => FakeSocket
  ): Promise<string | undefined>
  readConfig(
    env: Record<string, string | undefined>,
    argv?: string[]
  ): {
    endpoint: URL
    origin: string
    key: string
    folders: string[]
  }
  parseSse(text: string): unknown[]
  serialQueue(maxWaiting: number): {
    <T>(task: () => Promise<T>, token?: object): Promise<T | null>
    withdraw(token: object): boolean
  }
  MAX_WAITING_DEPLOYS: number
  MAX_IN_FLIGHT: number
  run(
    config: unknown,
    input: NodeJS.ReadableStream,
    output: NodeJS.WritableStream,
    log: (line: string) => void
  ): void
}

// Claude runs the extension with Node; use it when it is installed, so the
// suite exercises the real runtime, and Bun's Node compatibility otherwise.
const RUNTIME = Bun.which('node') ?? process.execPath

const bridge = createRequire(import.meta.url)(BRIDGE_PATH) as Bridge

const KEY = 'tk_test_secret_value_0123456789'

describe('bridge settings', () => {
  test('plain HTTP only to loopback and Tailscale addresses', () => {
    for (const ip of [
      '127.0.0.1',
      '127.1.2.3',
      '::1',
      '[::1]',
      '::ffff:127.0.0.1',
      '100.64.0.1',
      '100.127.255.254',
      'fd7a:115c:a1e0::1',
    ]) {
      expect(bridge.plainHttpAllowed(ip)).toBe(true)
    }
    for (const ip of [
      '192.168.1.10',
      '10.0.0.1',
      '100.63.255.255',
      '100.128.0.1',
      '8.8.8.8',
      'fd00::1',
      '::ffff:192.168.1.1',
      'localhost',
    ]) {
      expect(bridge.plainHttpAllowed(ip)).toBe(false)
    }
  })

  test('a Tailscale-range address is trusted only through the Tailscale interface', async () => {
    // A CGNAT address from the ISP sits on an interface with no address in
    // Tailscale's own IPv6 prefix; the Tailscale interface has one.
    const local = bridge.tailnetLocalAddresses({
      en0: [{ address: '100.72.1.9' }, { address: 'fe80::1%en0' }],
      utun4: [
        { address: '100.101.102.103' },
        { address: 'fd7a:115c:a1e0::1a' },
      ],
    })
    expect([...local].sort()).toEqual(['100.101.102.103', 'fd7a:115c:a1e0::1a'])
    expect(
      bridge.tailnetLocalAddresses({ en0: [{ address: '100.72.1.9' }] }).size
    ).toBe(0)

    const connectingFrom = (from: string) => () => {
      const socket = new EventEmitter() as FakeSocket
      socket.destroy = () => {}
      setTimeout(() => {
        socket.localAddress = from
        socket.emit('connect')
      }, 0)
      return socket
    }
    const origin = 'http://100.64.0.5:3000'
    // Reached through Tailscale: sent from (and bound to) the tailnet address.
    await expect(
      bridge.plainHttpSource(
        origin,
        '100.64.0.5',
        3000,
        local,
        connectingFrom('::ffff:100.101.102.103')
      )
    ).resolves.toBe('100.101.102.103')
    // Reached through the ISP's network: refused, nothing sent.
    await expect(
      bridge.plainHttpSource(
        origin,
        '100.64.0.5',
        3000,
        local,
        connectingFrom('100.72.1.9')
      )
    ).rejects.toThrow('does not reach it through Tailscale')
    // Not on a tailnet at all: refused without connecting.
    await expect(
      bridge.plainHttpSource(origin, '100.64.0.5', 3000, new Set(), () => {
        throw new Error('must not connect')
      })
    ).rejects.toThrow('does not reach it through Tailscale')
    // Loopback needs no check.
    await expect(
      bridge.plainHttpSource(
        'http://127.0.0.1:3000',
        '127.0.0.1',
        3000,
        new Set(),
        () => {
          throw new Error('must not connect')
        }
      )
    ).resolves.toBeUndefined()
  })

  test('the endpoint is always /mcp on the server origin', () => {
    const config = bridge.readConfig({
      TEMPS_URL: ' https://temps.example.com/some/page?q=1 ',
      TEMPS_API_KEY: `Bearer ${KEY}`,
    })
    expect(config.endpoint.href).toBe('https://temps.example.com/mcp?write=1')
    expect(config.key).toBe(KEY)
  })

  test('only full paths count as folders to deploy from; an empty setting allows none', () => {
    const env = { TEMPS_URL: 'https://temps.example.com', TEMPS_API_KEY: KEY }
    expect(bridge.readConfig(env).folders).toEqual([])
    expect(
      bridge.readConfig(env, [
        '/Users/me/code',
        '',
        '${user_config.deploy_folders}',
        'relative/dir',
        '/srv/apps',
      ]).folders
    ).toEqual(['/Users/me/code', '/srv/apps'])
  })

  test('started by the Temps CLI, it points at temps mcp add, not the extension', () => {
    expect(() =>
      bridge.readConfig({
        TEMPS_URL: 'https://temps.example.com',
        TEMPS_BRIDGE_HOST: 'cli',
      })
    ).toThrow('temps mcp add')
    expect(() =>
      bridge.readConfig({ TEMPS_URL: 'https://temps.example.com' })
    ).toThrow('the extension settings')
  })

  test('unusable settings are refused with what to change', () => {
    const cases: [Record<string, string>, string][] = [
      [{ TEMPS_API_KEY: KEY }, 'Set the Temps server address'],
      [{ TEMPS_URL: 'temps', TEMPS_API_KEY: KEY }, 'not a valid'],
      [
        { TEMPS_URL: 'ftp://temps.example.com', TEMPS_API_KEY: KEY },
        'https://',
      ],
      [
        { TEMPS_URL: 'https://me:pw@temps.example.com', TEMPS_API_KEY: KEY },
        'user name or password',
      ],
      [
        { TEMPS_URL: 'http://192.168.1.10:8080', TEMPS_API_KEY: KEY },
        'plain HTTP',
      ],
      [{ TEMPS_URL: 'https://temps.example.com' }, 'Set your Temps API key'],
      [
        {
          TEMPS_URL: 'https://temps.example.com',
          TEMPS_API_KEY: 'tk_a\r\nX-Evil: 1',
        },
        'Paste it again',
      ],
    ]
    for (const [env, message] of cases) {
      expect(() => bridge.readConfig(env)).toThrow(message)
    }
  })

  test('server-sent events carry one message each', () => {
    expect(
      bridge.parseSse(
        'event: message\ndata: {"id":1}\n\n: comment\n\ndata: {"id":\ndata: 2}\n\n'
      )
    ).toEqual([{ id: 1 }, { id: 2 }])
  })
})

type Handler = (req: Request) => Response | Promise<Response>

const servers: ReturnType<typeof Bun.serve>[] = []

function serve(handler: Handler) {
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: handler })
  servers.push(server)
  return server
}

afterEach(() => {
  for (const server of servers.splice(0)) server.stop(true)
})

/** Run the bridge, send it messages, and collect what it writes back. */
async function exchange(
  env: Record<string, string>,
  messages: unknown[],
  expectedReplies: number,
  entry = ENTRY_PATH,
  args: string[] = []
) {
  const child = Bun.spawn([RUNTIME, entry, ...args], {
    env: { PATH: process.env.PATH ?? '', ...env },
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  })
  const replies: unknown[] = []
  const reader = child.stdout.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  const done = (async () => {
    while (replies.length < expectedReplies) {
      const { value, done: finished } = await reader.read()
      if (finished) break
      buffer += decoder.decode(value)
      let newline
      while ((newline = buffer.indexOf('\n')) !== -1) {
        replies.push(JSON.parse(buffer.slice(0, newline)))
        buffer = buffer.slice(newline + 1)
      }
    }
  })()
  // One at a time, so a session header from the first reply is in place.
  for (const message of messages) {
    const before = replies.length
    child.stdin.write(`${JSON.stringify(message)}\n`)
    child.stdin.flush()
    const isRequest =
      typeof message === 'object' && message !== null && 'id' in message
    const deadline = Date.now() + 5000
    while (isRequest && replies.length === before && Date.now() < deadline) {
      await Bun.sleep(10)
    }
  }
  await Promise.race([done, Bun.sleep(5000)])
  child.stdin.end()
  child.kill()
  const stderr = await new Response(child.stderr).text()
  return { replies, stderr }
}

describe('bridge over stdio', () => {
  test('forwards messages with the key and answers with what Temps returns', async () => {
    const seen: {
      auth: string | null
      accept: string | null
      session: string | null
      body: unknown
      path: string
    }[] = []
    const server = serve(async (req) => {
      const body = await req.json()
      seen.push({
        auth: req.headers.get('authorization'),
        accept: req.headers.get('accept'),
        session: req.headers.get('mcp-session-id'),
        body,
        path: `${new URL(req.url).pathname}${new URL(req.url).search}`,
      })
      if (!('id' in body)) return new Response(null, { status: 202 })
      if (body.method === 'tools/list') {
        return new Response(
          `event: message\ndata: ${JSON.stringify({ jsonrpc: '2.0', id: body.id, result: { tools: [] } })}\n\n`,
          { headers: { 'content-type': 'text/event-stream' } }
        )
      }
      return Response.json(
        { jsonrpc: '2.0', id: body.id, result: { ok: true } },
        { headers: { 'mcp-session-id': 'session-1' } }
      )
    })

    const { replies } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [
        { jsonrpc: '2.0', id: 1, method: 'initialize', params: {} },
        { jsonrpc: '2.0', method: 'notifications/initialized' },
        { jsonrpc: '2.0', id: 2, method: 'tools/list' },
      ],
      2
    )

    expect(replies).toEqual([
      { jsonrpc: '2.0', id: 1, result: { ok: true } },
      {
        jsonrpc: '2.0',
        id: 2,
        result: {
          tools: [
            expect.objectContaining({
              name: 'temps_deploy_folder',
              annotations: expect.objectContaining({ destructiveHint: true }),
            }),
          ],
        },
      },
    ])
    // Always the write tools: the key's role decides what a write may do.
    expect(seen.map((s) => s.path)).toEqual([
      '/mcp?write=1',
      '/mcp?write=1',
      '/mcp?write=1',
    ])
    expect(seen.every((s) => s.auth === `Bearer ${KEY}`)).toBe(true)
    expect(seen[0].accept).toContain('text/event-stream')
    expect(seen[0].session).toBeNull()
    expect(seen[2].session).toBe('session-1')
  })

  test('it answers when a host loads it through a wrapper, and says it is ready', async () => {
    const server = serve(async (req) => {
      const body = await req.json()
      return Response.json({
        jsonrpc: '2.0',
        id: body.id,
        result: { ok: true },
      })
    })
    const { replies, stderr } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [{ jsonrpc: '2.0', id: 0, method: 'initialize', params: {} }],
      1,
      WRAPPED_PATH
    )
    expect(replies).toEqual([{ jsonrpc: '2.0', id: 0, result: { ok: true } }])
    expect(stderr).toContain(
      `temps: ready, forwarding MCP to http://127.0.0.1:${server.port}/mcp?write=1`
    )
    expect(stderr).toMatch(/temps: initialize answered in \d+ ms/)
    expect(stderr).not.toContain(KEY)
  })

  test('a rejected key is explained without echoing it', async () => {
    const server = serve(() =>
      Response.json(
        { title: 'Unauthorized', detail: 'bad key' },
        { status: 401 }
      )
    )
    const { replies, stderr } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [{ jsonrpc: '2.0', id: 7, method: 'initialize' }],
      1
    )
    expect(replies).toHaveLength(1)
    const reply = replies[0] as { id: number; error: { message: string } }
    expect(reply.id).toBe(7)
    expect(reply.error.message).toContain('rejected the API key')
    expect(JSON.stringify(replies)).not.toContain(KEY)
    expect(stderr).not.toContain(KEY)
  })

  test('a redirect is not followed, so the key never reaches the other address', async () => {
    let elsewhere = 0
    const other = serve(() => {
      elsewhere++
      return Response.json({ jsonrpc: '2.0', id: 1, result: {} })
    })
    const server = serve(
      () =>
        new Response(null, {
          status: 307,
          headers: { location: `http://127.0.0.1:${other.port}/mcp` },
        })
    )
    const { replies } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [{ jsonrpc: '2.0', id: 1, method: 'initialize' }],
      1
    )
    expect(
      (replies[0] as { error: { message: string } }).error.message
    ).toContain('does not follow redirects')
    expect(elsewhere).toBe(0)
  })

  test('a web page instead of the MCP endpoint is named as the wrong address', async () => {
    for (const [body, type] of [
      ['<!doctype html><title>Temps</title>', 'text/html; charset=utf-8'],
      ['not json at all', ''],
    ]) {
      const server = serve(
        () =>
          new Response(body, {
            headers: type ? { 'content-type': type } : {},
          })
      )
      const { replies } = await exchange(
        { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
        [{ jsonrpc: '2.0', id: 1, method: 'initialize' }],
        1
      )
      const message = (replies[0] as { error: { message: string } }).error
        .message
      expect(message).toContain("instead of Temps' MCP endpoint")
      expect(message).toContain('address of your Temps console')
      if (type) expect(message).toContain('text/html')
    }
  })

  test('the MCP server being off says who turns it on', async () => {
    const server = serve(() => new Response('', { status: 404 }))
    const { replies } = await exchange(
      { TEMPS_URL: `http://localhost:${server.port}`, TEMPS_API_KEY: KEY },
      [{ jsonrpc: '2.0', id: 1, method: 'initialize' }],
      1
    )
    expect(
      (replies[0] as { error: { message: string } }).error.message
    ).toContain('MCP server is off')
  })

  test('plain HTTP to a LAN address is refused, and every request says why', async () => {
    const { replies, stderr } = await exchange(
      { TEMPS_URL: 'http://192.168.1.10:8080', TEMPS_API_KEY: KEY },
      [
        { jsonrpc: '2.0', id: 1, method: 'initialize' },
        { jsonrpc: '2.0', method: 'notifications/initialized' },
        { jsonrpc: '2.0', id: 2, method: 'tools/list' },
      ],
      2
    )
    expect(replies.map((r) => (r as { id: number }).id)).toEqual([1, 2])
    for (const reply of replies) {
      expect((reply as { error: { message: string } }).error.message).toContain(
        'does not send your API key over plain HTTP'
      )
    }
    expect(stderr).not.toContain(KEY)
  })

  test('a message that is not JSON gets a parse error', async () => {
    const child = Bun.spawn([RUNTIME, ENTRY_PATH], {
      env: {
        PATH: process.env.PATH ?? '',
        TEMPS_URL: 'https://temps.example.com',
        TEMPS_API_KEY: KEY,
      },
      stdin: 'pipe',
      stdout: 'pipe',
      stderr: 'pipe',
    })
    child.stdin.write('not json\n')
    child.stdin.end()
    const out = await new Response(child.stdout).text()
    expect(JSON.parse(out.trim())).toEqual({
      jsonrpc: '2.0',
      id: null,
      error: {
        code: -32700,
        message: 'The extension received a message that is not JSON.',
      },
    })
  })
})

describe('folder deployments run one at a time', () => {
  test('each waits for the one before it, and a full queue refuses at once', async () => {
    const queue = bridge.serialQueue(2)
    let running = 0
    let most = 0
    const releases: Array<() => void> = []
    const task = (name: string) => () =>
      new Promise<string>((resolve) => {
        running++
        most = Math.max(most, running)
        releases.push(() => {
          running--
          resolve(name)
        })
      })
    const first = queue(task('first'))
    const second = queue(task('second'))
    const third = queue(task('third'))
    // The first runs; two wait; a fourth is past the queue.
    await expect(queue(task('fourth'))).rejects.toThrow(/already waiting/)
    const order: string[] = []
    for (const done of [first, second, third]) {
      while (releases.length === 0) await Bun.sleep(1)
      releases.shift()!()
      order.push((await done) ?? 'withdrawn')
    }
    expect(order).toEqual(['first', 'second', 'third'])
    expect(most).toBe(1)
    // A failed deployment does not stop the next.
    await expect(
      queue(() => Promise.reject(new Error('boom')))
    ).rejects.toThrow('boom')
    expect(await queue(() => Promise.resolve('after'))).toBe('after')
    expect(bridge.MAX_WAITING_DEPLOYS).toBeGreaterThan(0)
  })

  test('a waiting task withdrawn leaves its place to the next call and never runs', async () => {
    const queue = bridge.serialQueue(2)
    const ran: string[] = []
    const releases: Array<() => void> = []
    const task = (name: string) => () =>
      new Promise<string>((resolve) => {
        ran.push(name)
        releases.push(() => resolve(name))
      })
    const tokens = { first: {}, second: {}, third: {} }
    const first = queue(task('first'), tokens.first)
    const second = queue(task('second'), tokens.second)
    const third = queue(task('third'), tokens.third)
    await expect(queue(task('refused'))).rejects.toThrow(/2 folder deployments/)
    // The running one stays; a waiting one leaves, and its place is free.
    while (releases.length === 0) await Bun.sleep(1)
    expect(queue.withdraw(tokens.first)).toBe(false)
    expect(queue.withdraw(tokens.second)).toBe(true)
    expect(await second).toBeNull()
    const fourth = queue(task('fourth'))
    for (const done of [first, third, fourth]) {
      while (releases.length === 0) await Bun.sleep(1)
      releases.shift()!()
      await done
    }
    expect(ran).toEqual(['first', 'third', 'fourth'])
  })
})

describe('cancelling a folder deployment', () => {
  test('a call cancelled while preparing its archive sends nothing more to Temps', async () => {
    const deployModule = createRequire(import.meta.url)(DEPLOY_PATH) as {
      deployFolder: (...args: unknown[]) => Promise<string>
    }
    const original = deployModule.deployFolder
    let prepared = () => {}
    let started = false
    let upload: unknown = 'not attempted'
    // Like the real one: prepare the archive, then upload it through the
    // bridge's API callback.
    deployModule.deployFolder = async (_config, _args, api) => {
      started = true
      await new Promise<void>((resolve) => {
        prepared = resolve
      })
      try {
        upload = await (
          api as (
            v: string,
            p: string,
            h: object,
            b: string
          ) => Promise<unknown>
        )('POST', '/api/projects/1/deployments', {}, 'archive')
      } catch (error) {
        upload = error
      }
      return 'deployed'
    }
    const forwarded: unknown[] = []
    const server = serve(async (req) => {
      forwarded.push(await req.text())
      return new Response(null, { status: 202 })
    })
    try {
      const input = new PassThrough()
      const output = new PassThrough()
      let written = ''
      output.on('data', (chunk) => (written += String(chunk)))
      const config = bridge.readConfig({
        TEMPS_URL: `http://127.0.0.1:${server.port}`,
        TEMPS_API_KEY: KEY,
      })
      bridge.run(config, input, output, () => {})
      input.write(
        `${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'temps_deploy_folder', arguments: {} } })}\n`
      )
      while (!started) await Bun.sleep(1)
      input.write(
        `${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 1 } })}\n`
      )
      await Bun.sleep(10)
      prepared()
      await Bun.sleep(30)
      expect(upload).toBeInstanceOf(Error)
      expect(String(upload)).toMatch(/cancelled/)
      expect(forwarded).toEqual([])
      expect(written).toBe('')
      input.end()
    } finally {
      deployModule.deployFolder = original
      server.stop(true)
    }
  })

  test('calls past the in-flight limit wait unread, and all are answered in turn', async () => {
    let running = 0
    let most = 0
    const release: (() => void)[] = []
    const server = serve(async (req) => {
      const body = await req.json()
      running++
      most = Math.max(most, running)
      await new Promise<void>((resolve) => release.push(resolve))
      running--
      return Response.json({ jsonrpc: '2.0', id: body.id, result: {} })
    })
    try {
      const input = new PassThrough()
      const output = new PassThrough()
      let written = ''
      output.on('data', (chunk) => (written += String(chunk)))
      const config = bridge.readConfig({
        TEMPS_URL: `http://127.0.0.1:${server.port}`,
        TEMPS_API_KEY: KEY,
      })
      bridge.run(config, input, output, () => {})
      const total = bridge.MAX_IN_FLIGHT * 3
      input.write(
        Array.from(
          { length: total },
          (_, i) =>
            `${JSON.stringify({ jsonrpc: '2.0', id: i + 1, method: 'tools/list' })}\n`
        ).join('')
      )
      const answered = () => written.split('\n').filter(Boolean).length
      const deadline = Date.now() + 10000
      while (answered() < total && Date.now() < deadline) {
        while (release.length > 0) release.shift()?.()
        await Bun.sleep(5)
        expect(running).toBeLessThanOrEqual(bridge.MAX_IN_FLIGHT)
      }
      expect(answered()).toBe(total)
      expect(most).toBe(bridge.MAX_IN_FLIGHT)
      input.end()
    } finally {
      server.stop(true)
    }
  })

  test('a cancelled call still waiting never starts, gets no reply, and is not forwarded', async () => {
    const deployModule = createRequire(import.meta.url)(DEPLOY_PATH) as {
      deployFolder: (...args: unknown[]) => Promise<string>
    }
    const original = deployModule.deployFolder
    const started: unknown[] = []
    let releaseFirst = () => {}
    deployModule.deployFolder = (_config, args) => {
      started.push(args)
      return new Promise<string>((resolve) => {
        releaseFirst = () => resolve('deployed')
      })
    }
    // Anything forwarded would reach this server.
    const forwarded: unknown[] = []
    const server = serve(async (req) => {
      forwarded.push(await req.json())
      return new Response(null, { status: 202 })
    })
    try {
      const input = new PassThrough()
      const output = new PassThrough()
      let written = ''
      output.on('data', (chunk) => (written += String(chunk)))
      const config = bridge.readConfig({
        TEMPS_URL: `http://127.0.0.1:${server.port}`,
        TEMPS_API_KEY: KEY,
      })
      bridge.run(config, input, output, () => {})
      const call = (id: number) =>
        JSON.stringify({
          jsonrpc: '2.0',
          id,
          method: 'tools/call',
          params: { name: 'temps_deploy_folder', arguments: { n: id } },
        })
      input.write(`${call(1)}\n${call(2)}\n`)
      while (started.length === 0) await Bun.sleep(1)
      input.write(
        `${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 2 } })}\n`
      )
      await Bun.sleep(10)
      releaseFirst()
      await Bun.sleep(30)
      expect(started).toEqual([{ n: 1 }])
      const replies = written
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line) as { id: number })
      expect(replies.map((reply) => reply.id)).toEqual([1])
      expect(forwarded).toEqual([])
      input.end()
    } finally {
      deployModule.deployFolder = original
      server.stop(true)
    }
  })
})

describe('deploying a folder', () => {
  // realpath: on macOS the temp folder is behind a symlink.
  const allowed = realpathSync(mkdtempSync(join(tmpdir(), 'temps-allowed-')))
  const project = join(allowed, 'my-app')
  mkdirSync(join(project, 'src'), { recursive: true })
  writeFileSync(join(project, 'package.json'), '{"name":"my-app"}')
  writeFileSync(join(project, 'src', 'index.js'), 'export default 1\n')
  writeFileSync(join(project, '.env'), 'TOKEN=very-secret')
  afterAll(() => rmSync(allowed, { recursive: true, force: true }))

  const ENVIRONMENTS = [
    { id: 8, name: 'feature-x', slug: 'feature-x', is_preview: true },
    { id: 7, name: 'Production', slug: 'production', is_preview: false },
    { id: 9, name: 'staging', slug: 'staging', is_preview: false },
  ]

  /** A Temps server that records the API calls and accepts the upload. */
  function temps(deployStatus = 202, environments: unknown = ENVIRONMENTS) {
    const calls: { method: string; path: string; auth: string | null }[] = []
    const uploads: Map<string, { text: string; mode: number }>[] = []
    const server = serve(async (req) => {
      const url = new URL(req.url)
      calls.push({
        method: req.method,
        path: url.pathname,
        auth: req.headers.get('authorization'),
      })
      if (url.pathname === '/mcp') {
        const body = await req.json()
        return Response.json({
          jsonrpc: '2.0',
          id: body.id,
          result: { tools: [{ name: 'temps' }] },
        })
      }
      if (url.pathname === '/api/projects/4/environments') {
        return Response.json(environments)
      }
      if (url.pathname.startsWith('/api/projects/4/environments/')) {
        if (deployStatus !== 202) {
          return Response.json(
            {
              title: 'Forbidden',
              detail: 'requires the deployments:create permission',
            },
            { status: deployStatus }
          )
        }
        const form = await req.formData()
        const file = form.get('file') as File
        uploads.push(readDeflatedZip(new Uint8Array(await file.arrayBuffer())))
        return Response.json(
          {
            id: 42,
            project_id: 4,
            environment_id: 7,
            slug: 'deployment-42',
            state: 'pending',
            source_type: 'uploaded_source',
            created_at: '2026-11-25T10:00:00Z',
          },
          { status: 202 }
        )
      }
      return new Response('', { status: 404 })
    })
    return { server, calls, uploads }
  }

  const call = (id: number, args: Record<string, unknown>) => ({
    jsonrpc: '2.0',
    id,
    method: 'tools/call',
    params: { name: 'temps_deploy_folder', arguments: args },
  })

  type ToolReply = {
    id: number
    result: { content: { text: string }[]; isError: boolean }
  }

  test('one call uploads the folder to production and starts the deployment', async () => {
    const { server, calls, uploads } = temps()
    const { replies, stderr } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [call(1, { path: project, project_id: 4 })],
      1,
      ENTRY_PATH,
      [allowed]
    )
    const reply = replies[0] as ToolReply
    expect(reply.id).toBe(1)
    expect(reply.result.isError).toBe(false)
    const text = reply.result.content[0].text
    expect(text).toContain('Deployment 42 of project 4 (Production) started')
    expect(text).toContain('Uploaded 2 files')
    expect(text).toContain('Left out: .env.')
    // The tool never goes through the MCP endpoint, only these two routes.
    expect(calls.map((c) => `${c.method} ${c.path}`)).toEqual([
      'GET /api/projects/4/environments',
      'POST /api/projects/4/environments/7/deploy/source',
    ])
    expect(calls.every((c) => c.auth === `Bearer ${KEY}`)).toBe(true)
    expect([...uploads[0].keys()].sort()).toEqual([
      'package.json',
      'src/index.js',
    ])
    expect(uploads[0].get('src/index.js')?.text).toBe('export default 1\n')
    expect(stderr).toContain(`temps: may deploy from ${allowed}`)
    expect(stderr).toMatch(/temps: temps_deploy_folder answered in \d+ ms/)
    expect(stderr).not.toContain(KEY)
  })

  test('an environment named in the call is used as given', async () => {
    const { server, calls } = temps()
    const { replies } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [call(1, { path: project, project_id: '4', environment_id: 9 })],
      1,
      ENTRY_PATH,
      [allowed]
    )
    expect((replies[0] as ToolReply).result.isError).toBe(false)
    expect(calls.map((c) => c.path)).toEqual([
      '/api/projects/4/environments/9/deploy/source',
    ])
  })

  test('a folder outside the allowed ones, or none allowed, sends nothing', async () => {
    for (const [args, folders, message] of [
      [{ path: project, project_id: 4 }, [], 'Folders Claude may deploy'],
      [
        { path: tmpdir(), project_id: 4 },
        [allowed],
        'outside the folders Claude may deploy',
      ],
      [{ path: project }, [allowed], 'project_id'],
    ] as const) {
      const { server, calls } = temps()
      const { replies } = await exchange(
        { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
        [call(1, args)],
        1,
        ENTRY_PATH,
        [...folders]
      )
      const reply = replies[0] as ToolReply
      expect(reply.result.isError).toBe(true)
      expect(reply.result.content[0].text).toContain(message)
      expect(calls).toEqual([])
    }
  })

  test('a key that may not deploy is refused by Temps, and the reason is shown', async () => {
    const { server } = temps(403)
    const { replies } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [call(1, { path: project, project_id: 4 })],
      1,
      ENTRY_PATH,
      [allowed]
    )
    const reply = replies[0] as ToolReply
    expect(reply.result.isError).toBe(true)
    expect(reply.result.content[0].text).toContain(
      'The API key may not deploy project 4 (Production): requires the deployments:create permission'
    )
    expect(JSON.stringify(reply)).not.toContain(KEY)
  })

  test('with several environments and no production, it asks which one', async () => {
    const { server, calls } = temps(202, [
      { id: 9, name: 'staging', slug: 'staging', is_preview: false },
      { id: 10, name: 'qa', slug: 'qa', is_preview: false },
    ])
    const { replies } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [call(1, { path: project, project_id: 4 })],
      1,
      ENTRY_PATH,
      [allowed]
    )
    const reply = replies[0] as ToolReply
    expect(reply.result.isError).toBe(true)
    expect(reply.result.content[0].text).toContain(
      'Pass environment_id: staging (9), qa (10).'
    )
    expect(calls).toHaveLength(1)

    // An unknown project lists no environments.
    const none = temps(202, [])
    const { replies: unknown } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${none.server.port}`, TEMPS_API_KEY: KEY },
      [call(1, { path: project, project_id: 4 })],
      1,
      ENTRY_PATH,
      [allowed]
    )
    expect((unknown[0] as ToolReply).result.content[0].text).toContain(
      'or there is no project 4'
    )
  })

  test('the tool is listed once, after the tools Temps serves', async () => {
    const { server } = temps()
    const { replies } = await exchange(
      { TEMPS_URL: `http://127.0.0.1:${server.port}`, TEMPS_API_KEY: KEY },
      [{ jsonrpc: '2.0', id: 3, method: 'tools/list' }],
      1
    )
    const tools = (replies[0] as { result: { tools: { name: string }[] } })
      .result.tools
    expect(tools.map((t) => t.name)).toEqual(['temps', 'temps_deploy_folder'])
  })
})
