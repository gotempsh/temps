// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { spawn } from 'node:child_process'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import {
  LOCAL_TOOLS,
  CLI_PACKAGE,
  SERVER_VERSION,
  createServer,
  installGuide,
  normalizeBaseUrl,
  readConfig,
} from './bridge.js'

const PACKAGE_DIR = join(import.meta.dir, '..')
const REPO_ROOT = join(PACKAGE_DIR, '..', '..')

const CONFIGURED_ENV = {
  TEMPS_URL: 'https://temps.example.com',
  TEMPS_API_KEY: 'tk_test_key',
  TEMPS_MCP_WRITE: 'false',
}

function jsonResponse(status, body) {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
}

/** A fake Temps instance: records requests and answers from `routes`. */
function fakeTemps(routes) {
  const requests = []
  const fetchImpl = async (url, init = {}) => {
    const body = init.body ? JSON.parse(init.body) : null
    requests.push({ url: String(url), method: init.method ?? 'GET', headers: init.headers ?? {}, body })
    const handler = routes[`${init.method ?? 'GET'} ${new URL(url).pathname}`]
    if (!handler) return jsonResponse(404, { title: 'Not Found' })
    return handler(body, String(url))
  }
  return { fetchImpl, requests }
}

const REMOTE_TOOLS = [
  { name: 'list_projects', description: 'List projects', inputSchema: { type: 'object' } },
  { name: 'temps_setup_status', description: 'A remote tool must not shadow a local one', inputSchema: { type: 'object' } },
]

function healthyTemps() {
  return fakeTemps({
    'GET /mcp/tools': () => jsonResponse(200, { groups: [] }),
    'POST /mcp': (body) => {
      if (body.method === 'tools/list') return jsonResponse(200, { jsonrpc: '2.0', id: body.id, result: { tools: REMOTE_TOOLS } })
      if (body.method === 'tools/call') {
        return jsonResponse(200, {
          jsonrpc: '2.0',
          id: body.id,
          result: { content: [{ type: 'text', text: `called ${body.params.name}` }] },
        })
      }
      return jsonResponse(200, { jsonrpc: '2.0', id: body.id, error: { code: -32601, message: 'nope' } })
    },
  })
}

const rpc = (id, method, params) => ({ jsonrpc: '2.0', id, method, params })

describe('configuration', () => {
  test('normalizes a console or CLI-context URL to the server root', () => {
    expect(normalizeBaseUrl('https://temps.example.com/')).toEqual({ baseUrl: 'https://temps.example.com' })
    expect(normalizeBaseUrl('https://temps.example.com/api')).toEqual({ baseUrl: 'https://temps.example.com' })
    expect(normalizeBaseUrl('http://localhost:8080/api/')).toEqual({ baseUrl: 'http://localhost:8080' })
  })

  test('rejects URLs that would leak credentials or are not http(s)', () => {
    expect(normalizeBaseUrl('ftp://temps.example.com').error).toContain('must start with https://')
    expect(normalizeBaseUrl('https://user:pw@temps.example.com').error).toContain('must not contain credentials')
    expect(normalizeBaseUrl('https://temps.example.com/?key=1').error).toContain('query string')
    expect(normalizeBaseUrl('not a url').error).toContain('is not a valid URL')
  })

  test('treats blank fields and unexpanded ${user_config.x} placeholders as unset', () => {
    const config = readConfig({
      TEMPS_URL: '${user_config.temps_url}',
      TEMPS_API_KEY: '   ',
      TEMPS_MCP_WRITE: '${user_config.enable_write_tools}',
    })
    expect(config.configured).toBe(false)
    expect(config.baseUrl).toBeNull()
    expect(config.writeEnabled).toBe(false)
    expect(config.problems).toEqual(['No Temps console URL is configured.', 'No Temps API key is configured.'])
  })

  test('parses the write toggle', () => {
    expect(readConfig({ ...CONFIGURED_ENV, TEMPS_MCP_WRITE: 'true' }).writeEnabled).toBe(true)
    expect(readConfig({ ...CONFIGURED_ENV, TEMPS_MCP_WRITE: '0' }).writeEnabled).toBe(false)
  })
})

describe('protocol', () => {
  test('initialize echoes a supported protocol version and falls back otherwise', async () => {
    const server = createServer({ env: {}, fetchImpl: async () => { throw new Error('no network') } })
    const known = await server.handle(rpc(1, 'initialize', { protocolVersion: '2025-03-26' }))
    expect(known.result.protocolVersion).toBe('2025-03-26')
    expect(known.result.serverInfo).toEqual({ name: 'temps', version: SERVER_VERSION })
    const unknown = await server.handle(rpc(2, 'initialize', { protocolVersion: '1999-01-01' }))
    expect(unknown.result.protocolVersion).toBe('2025-06-18')
  })

  test('notifications get no response; unknown methods and bad JSON get JSON-RPC errors', async () => {
    const server = createServer({ env: {} })
    expect(await server.handle({ jsonrpc: '2.0', method: 'notifications/initialized' })).toBeNull()
    const unknown = await server.handle(rpc(3, 'resources/list'))
    expect(unknown.error.code).toBe(-32601)
    expect(unknown.error.message).toContain('resources/list')
    const parse = await server.handleLine('{not json')
    expect(parse.error.code).toBe(-32700)
    const invalid = await server.handle({ id: 4, method: 'ping' })
    expect(invalid.error.code).toBe(-32600)
  })
})

describe('unconfigured (before Temps is installed)', () => {
  test('lists the local onboarding tools without touching the network', async () => {
    let called = false
    const server = createServer({ env: {}, fetchImpl: async () => { called = true; throw new Error('unexpected') } })
    const response = await server.handle(rpc(1, 'tools/list'))
    expect(response.result.tools.map((tool) => tool.name)).toEqual(['temps_setup_status', 'temps_install_guide'])
    expect(called).toBe(false)
  })

  test('temps_setup_status explains what is missing and how to fix it', async () => {
    const server = createServer({ env: {} })
    const response = await server.handle(rpc(2, 'tools/call', { name: 'temps_setup_status', arguments: {} }))
    const status = JSON.parse(response.result.content[0].text)
    expect(status.configured).toBe(false)
    expect(status.problems).toContain('No Temps console URL is configured.')
    expect(status.next_steps.join(' ')).toContain('temps_install_guide')
  })

  test('a remote tool fails loudly instead of silently doing nothing', async () => {
    const server = createServer({ env: {} })
    const response = await server.handle(rpc(3, 'tools/call', { name: 'list_projects', arguments: {} }))
    expect(response.result.isError).toBe(true)
    expect(response.result.content[0].text).toContain('not connected to a Temps instance')
    expect(response.result.content[0].text).toContain('temps_setup_status')
  })
})

describe('temps_install_guide', () => {
  test('this_computer uses temps start from the documented CLI channel', () => {
    const guide = installGuide({ target: 'this_computer' })
    expect(guide).toContain(`bunx ${CLI_PACKAGE} start --email you@example.com`)
    expect(guide).toContain('Docker')
    expect(guide).toContain('Settings → Extensions → Temps')
  })

  test('servers preview the SSH install with --dry-run before running it', () => {
    for (const target of ['existing_server', 'new_server']) {
      const guide = installGuide({ target, has_domain: false })
      const dryRun = guide.indexOf('--dry-run')
      expect(dryRun).toBeGreaterThan(-1)
      expect(guide.indexOf(`bunx ${CLI_PACKAGE} setup --ssh`)).toBeLessThan(dryRun)
      expect(guide).toContain('sslip.io')
      expect(guide).toContain('never paste them into a chat')
    }
    expect(installGuide({ target: 'new_server' })).toContain('billed by that provider')
    expect(installGuide({ target: 'existing_server', has_domain: true })).toContain('advanced mode')
  })

  test('never pipes a download into a shell', () => {
    for (const target of ['this_computer', 'existing_server', 'new_server']) {
      expect(installGuide({ target })).not.toMatch(/\|\s*(ba)?sh\b/)
    }
  })

  test('rejects an unknown target with the accepted values', async () => {
    const server = createServer({ env: {} })
    const response = await server.handle(rpc(1, 'tools/call', { name: 'temps_install_guide', arguments: { target: 'mars' } }))
    expect(response.result.isError).toBe(true)
    expect(response.result.content[0].text).toContain('this_computer, existing_server, new_server')
  })
})

describe('connected to a Temps instance', () => {
  test('merges remote tools after the local ones without letting them shadow local names', async () => {
    const { fetchImpl, requests } = healthyTemps()
    const server = createServer({ env: CONFIGURED_ENV, fetchImpl })
    const response = await server.handle(rpc(1, 'tools/list'))
    expect(response.result.tools.map((tool) => tool.name)).toEqual(['temps_setup_status', 'temps_install_guide', 'list_projects'])
    expect(requests[0].url).toBe('https://temps.example.com/mcp')
    expect(requests[0].headers.authorization).toBe('Bearer tk_test_key')
  })

  test('forwards tool calls and asks for write tools only when enabled', async () => {
    const { fetchImpl, requests } = healthyTemps()
    const server = createServer({ env: { ...CONFIGURED_ENV, TEMPS_MCP_WRITE: 'true' }, fetchImpl })
    const response = await server.handle(rpc(7, 'tools/call', { name: 'list_projects', arguments: { page: 1 } }))
    expect(response.id).toBe(7)
    expect(response.result.content[0].text).toBe('called list_projects')
    expect(requests[0].url).toBe('https://temps.example.com/mcp?write=1')
    expect(requests[0].body.params).toEqual({ name: 'list_projects', arguments: { page: 1 } })
  })

  test('reports a healthy connection', async () => {
    const { fetchImpl } = healthyTemps()
    const server = createServer({ env: CONFIGURED_ENV, fetchImpl })
    const response = await server.handle(rpc(1, 'tools/call', { name: 'temps_setup_status', arguments: {} }))
    const status = JSON.parse(response.result.content[0].text)
    expect(status).toMatchObject({ configured: true, mcp_server: 'enabled', authenticated: true, problems: [] })
    expect(status.remote_tools).toContain('list_projects')
  })

  test('a disabled MCP server points the admin at the settings page', async () => {
    const { fetchImpl } = fakeTemps({})
    const server = createServer({ env: CONFIGURED_ENV, fetchImpl })
    const status = JSON.parse(
      (await server.handle(rpc(1, 'tools/call', { name: 'temps_setup_status', arguments: {} }))).result.content[0].text,
    )
    expect(status.mcp_server).toBe('disabled')
    expect(status.next_steps.join(' ')).toContain('https://temps.example.com/settings/mcp-server')

    const call = await server.handle(rpc(2, 'tools/call', { name: 'list_projects', arguments: {} }))
    expect(call.result.isError).toBe(true)
    expect(call.result.content[0].text).toContain('not enabled on https://temps.example.com')
  })

  test('a rejected key says which instance rejected it and where to make a new one', async () => {
    const { fetchImpl } = fakeTemps({
      'GET /mcp/tools': () => jsonResponse(200, { groups: [] }),
      'POST /mcp': () => jsonResponse(401, { title: 'Unauthorized' }),
    })
    const server = createServer({ env: CONFIGURED_ENV, fetchImpl })
    const status = JSON.parse(
      (await server.handle(rpc(1, 'tools/call', { name: 'temps_setup_status', arguments: {} }))).result.content[0].text,
    )
    expect(status.authenticated).toBe(false)
    expect(status.problems.join(' ')).toContain('https://temps.example.com/settings/keys')
  })

  test('an unreachable instance degrades tools/list to the local tools', async () => {
    const logs = []
    const server = createServer({
      env: CONFIGURED_ENV,
      fetchImpl: async () => { throw Object.assign(new TypeError('fetch failed'), { cause: { code: 'ECONNREFUSED' } }) },
      log: (entry) => logs.push(entry),
    })
    const response = await server.handle(rpc(1, 'tools/list'))
    expect(response.result.tools.map((tool) => tool.name)).toEqual(LOCAL_TOOLS.map((tool) => tool.name))
    expect(logs.some((entry) => entry.level === 'warn' && entry.error.includes('ECONNREFUSED'))).toBe(true)
  })

  test('flags plain HTTP to a non-local host', async () => {
    const { fetchImpl } = healthyTemps()
    const server = createServer({ env: { ...CONFIGURED_ENV, TEMPS_URL: 'http://temps.example.com' }, fetchImpl })
    const status = JSON.parse(
      (await server.handle(rpc(1, 'tools/call', { name: 'temps_setup_status', arguments: {} }))).result.content[0].text,
    )
    expect(status.problems.join(' ')).toContain('plain HTTP')
  })

  test('never logs the API key', async () => {
    const logs = []
    const { fetchImpl } = healthyTemps()
    const server = createServer({ env: CONFIGURED_ENV, fetchImpl, log: (entry) => logs.push(entry) })
    await server.handle(rpc(1, 'tools/list'))
    expect(JSON.stringify(logs)).not.toContain('tk_test_key')
  })
})

describe('bundle', () => {
  test('manifest, package.json and server agree on the version and declare every local tool', () => {
    const manifest = JSON.parse(readFileSync(join(PACKAGE_DIR, 'manifest.json'), 'utf8'))
    const pkg = JSON.parse(readFileSync(join(PACKAGE_DIR, 'package.json'), 'utf8'))
    expect(manifest.version).toBe(SERVER_VERSION)
    expect(pkg.version).toBe(SERVER_VERSION)
    const declared = manifest.tools.map((tool) => tool.name)
    for (const tool of LOCAL_TOOLS) expect(declared).toContain(tool.name)
    expect(manifest.user_config.api_key.sensitive).toBe(true)
    expect(manifest.server.mcp_config.env.TEMPS_API_KEY).toBe('${user_config.api_key}')
  })

  test('guides use the same CLI channel as the Temps skills', () => {
    const runtime = readFileSync(join(REPO_ROOT, 'skills', 'temps', 'references', 'cli-runtime.md'), 'utf8')
    const channel = runtime.match(/bunx (@temps-sdk\/cli@(?:latest|beta))\b/)
    expect(channel?.[1]).toBe(CLI_PACKAGE)
  })

  test('the stdio entry point answers initialize and exits when stdin closes', async () => {
    const child = spawn(process.execPath, [join(import.meta.dir, 'index.js')], {
      env: { PATH: process.env.PATH ?? '' },
      stdio: ['pipe', 'pipe', 'pipe'],
    })
    let stdout = ''
    child.stdout.on('data', (chunk) => { stdout += chunk })
    child.stdin.end(`${JSON.stringify(rpc(1, 'initialize', { protocolVersion: '2025-06-18' }))}\n`)
    const code = await new Promise((resolve) => child.on('close', resolve))
    expect(code).toBe(0)
    const lines = stdout.trim().split('\n')
    expect(lines).toHaveLength(1)
    expect(JSON.parse(lines[0]).result.serverInfo.name).toBe('temps')
  })
})

describe('pack.sh', () => {
  test('ships every server module and no tests', () => {
    const packScript = readFileSync(join(PACKAGE_DIR, 'pack.sh'), 'utf8')
    const listed = [...packScript.matchAll(/^ {2}(server\/\S+)$/gm)].map((match) => match[1]).sort()
    const modules = readdirSync(join(PACKAGE_DIR, 'server'))
      .filter((file) => file.endsWith('.js') && !file.endsWith('.test.js'))
      .map((file) => `server/${file}`)
      .sort()
    expect(listed).toEqual(modules)
  })
})
