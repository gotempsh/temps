// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Temps for the Claude app: the server of the Temps desktop extension.
 *
 * Claude runs it on the user's computer and talks MCP to it over stdio. It
 * forwards each message to the Temps MCP endpoint (`<server>/mcp`) with the
 * user's API key and writes back what Temps answers. Running on the computer,
 * it reaches a console on localhost or a tailnet that Claude's cloud cannot.
 *
 * It also answers one tool itself, `temps_deploy_folder` (`deploy.cjs`):
 * it packs a project folder on this computer, inside a folder the user
 * allowed, and uploads it to Temps, which builds and deploys it.
 *
 * It holds a credential, so it is strict about where that goes:
 * - The key is only ever sent to the server address the user set: its
 *   `/mcp` endpoint, and for `temps_deploy_folder` the project's
 *   environments list and source deploy route. Redirects are not followed.
 * - Plain HTTP is refused unless every address the server name resolves to
 *   is loopback or Tailscale (WireGuard encrypts tailnet traffic), and the
 *   connection goes to the address that was checked, so DNS cannot swap it.
 * - HTTPS certificates are verified as usual.
 * - The key never appears in output or logs.
 * - Messages and responses are size-capped, and requests time out.
 *
 * It always asks for the write tools (`?write=1`). What a write may do is the
 * key's role: Temps proposes each write, executes it only through
 * `confirm_action`, and checks the key's permissions again at that moment,
 * so a read-only key is refused there.
 *
 * No dependencies: the extension is a manifest, this file and `deploy.cjs`.
 */

'use strict'

const http = require('node:http')
const https = require('node:https')
const dns = require('node:dns')
const net = require('node:net')
const nodePath = require('node:path')
const deploy = require('./deploy.cjs')

const MAX_MESSAGE_BYTES = 8 * 1024 * 1024
const REQUEST_TIMEOUT_MS = 5 * 60 * 1000
const CONNECT_TIMEOUT_MS = 10 * 1000
const USER_AGENT = 'temps-claude-extension/1'

/** Whether an IP address is loopback or on a tailnet. */
function plainHttpAllowed(address) {
  let ip = String(address).toLowerCase()
  if (ip.startsWith('[') && ip.endsWith(']')) ip = ip.slice(1, -1)
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/.exec(ip)
  if (mapped) ip = mapped[1]
  if (net.isIPv4(ip)) {
    const [a, b] = ip.split('.').map(Number)
    return a === 127 || (a === 100 && b >= 64 && b <= 127)
  }
  if (net.isIPv6(ip)) {
    return ip === '::1' || ip.startsWith('fd7a:115c:a1e0:')
  }
  return false
}

/**
 * The folders the user allowed `temps_deploy_folder` to deploy from: the
 * extension's arguments after the entry file. A setting left empty may arrive
 * as an empty or unexpanded value; those allow nothing.
 */
function allowedFolders(argv) {
  return (argv || []).filter(
    (folder) =>
      typeof folder === 'string' &&
      folder.trim() !== '' &&
      !folder.includes('${') &&
      nodePath.isAbsolute(folder)
  )
}

/**
 * The bridge's settings, checked. Throws an Error whose message tells the
 * user what to change, in the words of the host that started it.
 */
function readConfig(env, argv) {
  const host = deploy.hostFor(env)
  const raw = String(env.TEMPS_URL || '').trim()
  if (!raw) {
    throw new Error(
      `Set the Temps server address in ${host.settings}, for example https://temps.example.com.`
    )
  }
  let url
  try {
    url = new URL(raw)
  } catch {
    throw new Error(`"${raw}" is not a valid Temps server address.`)
  }
  if (url.protocol !== 'https:' && url.protocol !== 'http:') {
    throw new Error(
      `The Temps server address must start with https://, not ${url.protocol}//.`
    )
  }
  if (url.username || url.password) {
    throw new Error(
      'The Temps server address must not contain a user name or password. Put the API key in its own setting.'
    )
  }
  const hostname = url.hostname.replace(/^\[|\]$/g, '')
  if (
    url.protocol === 'http:' &&
    net.isIP(hostname) &&
    !plainHttpAllowed(hostname)
  ) {
    throw new Error(plainHttpRefusal(url.origin))
  }

  let key = String(env.TEMPS_API_KEY || '').trim()
  if (/^bearer\s+/i.test(key)) key = key.replace(/^bearer\s+/i, '')
  if (!key) {
    throw new Error(`Set your Temps API key in ${host.settings}.`)
  }
  if (!/^[\x21-\x7e]+$/.test(key)) {
    throw new Error(
      'The Temps API key contains spaces or characters a key never has. Paste it again.'
    )
  }
  const endpoint = new URL('/mcp', url.origin)
  endpoint.searchParams.set('write', '1')
  return {
    endpoint,
    origin: url.origin,
    key,
    folders: allowedFolders(argv),
    host,
  }
}

function plainHttpRefusal(origin) {
  return `The extension does not send your API key over plain HTTP to ${origin}: only to this computer or a Tailscale address. Serve Temps over HTTPS, or reach it through Tailscale, and update the server address.`
}

/** The addresses to try for an http:// endpoint, after checking every address the name resolves to. */
async function resolvePlainHttp(endpoint) {
  const host = endpoint.hostname.replace(/^\[|\]$/g, '')
  const addresses = net.isIP(host)
    ? [{ address: host }]
    : await dns.promises.lookup(host, { all: true, verbatim: true })
  if (
    addresses.length === 0 ||
    !addresses.every((a) => plainHttpAllowed(a.address))
  ) {
    throw new Error(plainHttpRefusal(endpoint.origin))
  }
  return addresses.map((a) => a.address)
}

// Failures before a connection exists: nothing was sent, so the next address
// may be tried ("localhost" is often ::1 first while a server listens on
// 127.0.0.1 only).
const UNREACHABLE = new Set([
  'ETIMEDOUT',
  'ECONNREFUSED',
  'EHOSTUNREACH',
  'ENETUNREACH',
  'EADDRNOTAVAIL',
])

/**
 * Send one request to the Temps server with the key. `path` is built here,
 * never taken from a message, so the key only goes to this server's MCP
 * endpoint and the two API routes `temps_deploy_folder` uses. Resolves to the
 * status, headers and body text.
 */
async function send(config, method, path, headers, body) {
  if (!path.startsWith('/') || path.startsWith('//')) {
    throw new Error(`Refusing to send the key to ${path}.`)
  }
  const request = { method, path, headers: headers || {}, body }
  if (config.endpoint.protocol === 'https:') {
    return sendTo(
      config,
      config.endpoint.hostname.replace(/^\[|\]$/g, ''),
      request
    )
  }
  const addresses = await resolvePlainHttp(config.endpoint)
  for (let i = 0; ; i++) {
    try {
      return await sendTo(config, addresses[i], request)
    } catch (error) {
      const code = error && typeof error === 'object' ? error.code : undefined
      if (i + 1 >= addresses.length || !UNREACHABLE.has(code)) throw error
    }
  }
}

/** POST one MCP message to Temps. */
function post(config, body, sessionId) {
  const headers = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
  }
  if (sessionId) headers['mcp-session-id'] = sessionId
  const { endpoint } = config
  return send(
    config,
    'POST',
    `${endpoint.pathname}${endpoint.search}`,
    headers,
    body
  )
}

async function sendTo(config, hostname, request) {
  const { endpoint } = config
  const secure = endpoint.protocol === 'https:'
  const headers = {
    ...request.headers,
    authorization: `Bearer ${config.key}`,
    'user-agent': USER_AGENT,
    host: endpoint.host,
  }
  if (request.body !== undefined) {
    headers['content-length'] = Buffer.byteLength(request.body)
  }
  const options = {
    method: request.method,
    protocol: endpoint.protocol,
    hostname,
    port: endpoint.port || (secure ? 443 : 80),
    path: request.path,
    headers,
    timeout: REQUEST_TIMEOUT_MS,
    agent: false,
  }

  return new Promise((resolve, reject) => {
    const req = (secure ? https : http).request(options, (res) => {
      const chunks = []
      let size = 0
      res.on('data', (chunk) => {
        size += chunk.length
        if (size > MAX_MESSAGE_BYTES) {
          req.destroy(
            new Error(
              `Temps sent a response larger than ${MAX_MESSAGE_BYTES} bytes; it was dropped.`
            )
          )
          return
        }
        chunks.push(chunk)
      })
      res.on('end', () =>
        resolve({
          status: res.statusCode || 0,
          headers: res.headers,
          text: Buffer.concat(chunks).toString('utf8'),
        })
      )
      res.on('error', reject)
    })
    // A connection that neither opens nor fails (a firewall dropping
    // packets) gives up in time for the next address and for Claude, which
    // waits about a minute for its first answer.
    req.on('socket', (socket) => {
      if (!socket.connecting) return
      const timer = setTimeout(() => {
        const error = new Error(
          `Could not connect to ${config.origin} within ${CONNECT_TIMEOUT_MS / 1000} seconds.`
        )
        error.code = 'ETIMEDOUT'
        req.destroy(error)
      }, CONNECT_TIMEOUT_MS)
      socket.once('connect', () => clearTimeout(timer))
      socket.once('close', () => clearTimeout(timer))
    })
    req.on('timeout', () =>
      req.destroy(
        new Error(
          `Temps did not answer within ${REQUEST_TIMEOUT_MS / 1000} seconds.`
        )
      )
    )
    req.on('error', reject)
    req.end(request.body)
  })
}

/** The JSON messages in a text/event-stream body. */
function parseSse(text) {
  const messages = []
  for (const event of text.split(/\r?\n\r?\n/)) {
    const data = event
      .split(/\r?\n/)
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).replace(/^ /, ''))
      .join('\n')
    if (data) messages.push(JSON.parse(data))
  }
  return messages
}

/** What a failed HTTP status means for the user, without echoing the key. */
function statusError(status, text, origin, host) {
  if (status === 401) return deploy.rejectedKey(origin, host)
  if (status === 403) {
    return `The API key cannot read projects on ${origin}. Use a key with at least the reader role.`
  }
  if (status === 404) {
    return `The MCP server is off on ${origin}, or the address is not a Temps console. An admin turns it on in the Temps console under Settings, MCP server.`
  }
  let detail = ''
  try {
    const problem = JSON.parse(text)
    detail = problem.detail || problem.title || ''
  } catch {
    // Not a problem document; the status alone says it.
  }
  return `Temps at ${origin} answered ${status}${detail ? `: ${String(detail).slice(0, 500)}` : '.'}`
}

/** Forward one message; returns the messages to write back. */
async function forward(config, message, session) {
  const res = await post(config, JSON.stringify(message), session.id)
  const sessionHeader = res.headers['mcp-session-id']
  if (typeof sessionHeader === 'string' && sessionHeader)
    session.id = sessionHeader

  if (res.status >= 300 && res.status < 400) {
    throw new Error(
      `Temps at ${config.origin} redirected the request. The extension does not follow redirects, so your key only goes to the address you set. Set the server address to the console's final address.`
    )
  }
  if (res.status === 202 || res.status === 204) return []
  if (res.status < 200 || res.status >= 300) {
    throw new Error(
      statusError(res.status, res.text, config.origin, config.host)
    )
  }
  const type = String(res.headers['content-type'] || '').toLowerCase()
  if (type.includes('text/event-stream')) return parseSse(res.text)
  if (!res.text.trim()) return []
  // Anything but JSON here is not the MCP endpoint: most often a web page,
  // from an address that serves a site (or a dev server) instead of Temps.
  const notMcp = () =>
    new Error(
      `${config.origin}/mcp answered with ${type ? type.split(';')[0] : 'something'} instead of Temps' MCP endpoint. Set the server address to the address of your Temps console.`
    )
  if (type && !type.includes('json')) throw notMcp()
  let body
  try {
    body = JSON.parse(res.text)
  } catch {
    throw notMcp()
  }
  return Array.isArray(body) ? body : [body]
}

function errorReply(id, code, message) {
  return { jsonrpc: '2.0', id, error: { code, message } }
}

function toolResult(id, text, isError) {
  return {
    jsonrpc: '2.0',
    id,
    result: { content: [{ type: 'text', text }], isError },
  }
}

/** Whether a message calls the tool this bridge answers itself. */
function isDeployCall(message) {
  return (
    message.method === 'tools/call' &&
    message.params &&
    message.params.name === deploy.DEPLOY_TOOL_NAME
  )
}

/**
 * Add the deploy tool to the first page of the server's tool list, so Claude
 * sees it with the others.
 */
function withDeployTool(message, replies) {
  if (
    !message ||
    message.method !== 'tools/list' ||
    (message.params && message.params.cursor)
  ) {
    return replies
  }
  return replies.map((reply) => {
    const tools = reply && reply.result && reply.result.tools
    if (reply.id !== message.id || !Array.isArray(tools)) return reply
    if (tools.some((tool) => tool && tool.name === deploy.DEPLOY_TOOL_NAME)) {
      return reply
    }
    return {
      ...reply,
      result: { ...reply.result, tools: [...tools, deploy.DEPLOY_TOOL] },
    }
  })
}

/**
 * Run the bridge over the given streams. `config` is either the settings or
 * the Error that explains why they are unusable; then every request is
 * answered with that explanation, so Claude shows it instead of a dead server.
 */
function run(config, input, output, log) {
  const session = { id: null }
  let buffer = ''
  let discarding = false
  let pending = 0
  let ended = false

  const write = (message) => output.write(`${JSON.stringify(message)}\n`)
  const maybeExit = () => {
    if (ended && pending === 0 && output !== process.stdout) output.end()
  }

  const handle = async (line) => {
    let message
    try {
      message = JSON.parse(line)
    } catch {
      write(
        errorReply(
          null,
          -32700,
          'The extension received a message that is not JSON.'
        )
      )
      return
    }
    const id =
      message && typeof message === 'object' && 'id' in message
        ? message.id
        : undefined
    const isRequest = id !== undefined && id !== null
    const method =
      message && typeof message.method === 'string' ? message.method : 'reply'
    const started = Date.now()
    if (isRequest && isDeployCall(message)) {
      // Answered here: a tool error is a result Claude reads, not a failure.
      try {
        if (config instanceof Error) throw config
        const text = await deploy.deployFolder(
          config,
          message.params.arguments,
          (verb, path, headers, body) => send(config, verb, path, headers, body)
        )
        write(toolResult(id, text, false))
      } catch (error) {
        const text = error instanceof Error ? error.message : String(error)
        write(toolResult(id, text, true))
      }
      log(
        `temps: ${deploy.DEPLOY_TOOL_NAME} answered in ${Date.now() - started} ms`
      )
      return
    }
    try {
      if (config instanceof Error) throw config
      const replies = withDeployTool(
        message,
        await forward(config, message, session)
      )
      for (const reply of replies) write(reply)
      log(`temps: ${method} answered in ${Date.now() - started} ms`)
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error)
      log(`temps: ${text}`)
      if (isRequest) write(errorReply(id, -32000, text))
    }
  }

  input.setEncoding('utf8')
  input.on('data', (chunk) => {
    buffer += chunk
    let newline
    while ((newline = buffer.indexOf('\n')) !== -1) {
      const line = buffer.slice(0, newline).trim()
      buffer = buffer.slice(newline + 1)
      if (discarding) {
        discarding = false
        continue
      }
      if (!line) continue
      pending++
      handle(line).finally(() => {
        pending--
        maybeExit()
      })
    }
    if (Buffer.byteLength(buffer) > MAX_MESSAGE_BYTES) {
      log(`temps: dropped a message larger than ${MAX_MESSAGE_BYTES} bytes`)
      buffer = ''
      discarding = true
    }
  })
  input.on('end', () => {
    ended = true
    maybeExit()
  })
}

/**
 * Start the bridge on this process's stdio. The extension's entry file calls
 * this unconditionally: the Claude app may load that file through a wrapper
 * of its own, so a `require.main === module` check would never run it and
 * every request would time out with no output at all. `temps mcp bridge`
 * calls it with its own flags in place of the environment and arguments.
 */
function main(env = process.env, argv = process.argv.slice(2)) {
  const log = (line) => process.stderr.write(`${line}\n`)
  let config
  try {
    config = readConfig(env, argv)
    log(`temps: ready, forwarding MCP to ${config.endpoint.href}`)
    log(
      config.folders.length > 0
        ? `temps: may deploy from ${config.folders.join(', ')}`
        : 'temps: no folder may be deployed yet'
    )
  } catch (error) {
    config = error instanceof Error ? error : new Error(String(error))
    log(`temps: ${config.message}`)
  }
  run(config, process.stdin, process.stdout, log)
}

module.exports = {
  plainHttpAllowed,
  readConfig,
  parseSse,
  statusError,
  run,
  main,
}
