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
 *   A Tailscale address must also be reached through this computer's
 *   Tailscale interface: `100.64.0.0/10` is shared address space that ISPs
 *   (CGNAT) and other networks use too, where nothing encrypts it.
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
const os = require('node:os')
const nodePath = require('node:path')
const deploy = require('./deploy.cjs')

const MAX_MESSAGE_BYTES = 8 * 1024 * 1024
const REQUEST_TIMEOUT_MS = 5 * 60 * 1000
const CONNECT_TIMEOUT_MS = 10 * 1000
const USER_AGENT = 'temps-claude-extension/1'

/** An address as compared here: no brackets, lower case, IPv4-mapped unwrapped. */
function normalizeIp(address) {
  let ip = String(address).toLowerCase()
  if (ip.startsWith('[') && ip.endsWith(']')) ip = ip.slice(1, -1)
  const zone = ip.indexOf('%')
  if (zone !== -1) ip = ip.slice(0, zone)
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/.exec(ip)
  return mapped ? mapped[1] : ip
}

function isLoopback(address) {
  const ip = normalizeIp(address)
  if (net.isIPv4(ip)) return ip.split('.')[0] === '127'
  return ip === '::1'
}

/** Tailscale's own IPv6 prefix, which no other network hands out. */
function inTailscaleIpv6(ip) {
  return net.isIPv6(ip) && ip.startsWith('fd7a:115c:a1e0:')
}

/**
 * Whether an IP address is loopback or in a range Tailscale uses. A
 * Tailscale-range address is only a candidate: `send` then checks that this
 * computer reaches it through its Tailscale interface.
 */
function plainHttpAllowed(address) {
  const ip = normalizeIp(address)
  if (isLoopback(ip)) return true
  if (net.isIPv4(ip)) {
    const [a, b] = ip.split('.').map(Number)
    return a === 100 && b >= 64 && b <= 127
  }
  return inTailscaleIpv6(ip)
}

/**
 * This computer's addresses on its Tailscale interface: every address of an
 * interface that holds one in Tailscale's own IPv6 prefix. A CGNAT address
 * an ISP hands out is on an interface without one, so it is not here.
 */
function tailnetLocalAddresses(interfaces = os.networkInterfaces()) {
  const local = new Set()
  for (const addresses of Object.values(interfaces || {})) {
    const list = (addresses || []).map((a) => normalizeIp(a.address))
    if (list.some(inTailscaleIpv6)) for (const ip of list) local.add(ip)
  }
  return local
}

function notThroughTailscale(origin, address) {
  return `The extension does not send your API key over plain HTTP to ${origin}: ${address} is in Tailscale's range, but this computer does not reach it through Tailscale, and other networks (an ISP's CGNAT) use that range unencrypted. Connect this computer to the tailnet, or serve Temps over HTTPS.`
}

/**
 * The local address to send from, to reach `address` over plain HTTP: the
 * one the system picks for it, which must be on the Tailscale interface,
 * so the traffic is WireGuard's. Loopback needs none. A test connection
 * shows what the system picks; the request is then bound to it.
 */
async function plainHttpSource(
  origin,
  address,
  port,
  local = tailnetLocalAddresses(),
  connect = net.connect
) {
  if (isLoopback(address)) return undefined
  if (local.size === 0) throw new Error(notThroughTailscale(origin, address))
  const source = await new Promise((resolve, reject) => {
    const socket = connect({ host: address, port })
    const timer = setTimeout(() => {
      socket.destroy()
      const error = new Error(
        `Could not connect to ${origin} within ${CONNECT_TIMEOUT_MS / 1000} seconds.`
      )
      error.code = 'ETIMEDOUT'
      reject(error)
    }, CONNECT_TIMEOUT_MS)
    socket.once('connect', () => {
      clearTimeout(timer)
      const from = socket.localAddress
      socket.destroy()
      resolve(from)
    })
    socket.once('error', (error) => {
      clearTimeout(timer)
      socket.destroy()
      reject(error)
    })
  })
  const from = normalizeIp(source || '')
  if (!local.has(from)) throw new Error(notThroughTailscale(origin, address))
  return from
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
  const port = Number(config.endpoint.port || 80)
  for (let i = 0; ; i++) {
    try {
      const source = await plainHttpSource(config.origin, addresses[i], port)
      return await sendTo(config, addresses[i], request, source)
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

async function sendTo(config, hostname, request, localAddress) {
  const { endpoint } = config
  const limit = config.requestTimeoutMs ?? REQUEST_TIMEOUT_MS
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
    timeout: limit,
    agent: false,
  }
  if (localAddress) options.localAddress = localAddress

  return new Promise((resolve, reject) => {
    // The socket's timeout counts only silence: a stream that keeps
    // sending (SSE keepalives) would hold its request, and its slot, for
    // ever. The whole exchange ends at the limit, however busy.
    const deadline = setTimeout(() => {
      req.destroy(
        new Error(`Temps did not answer within ${limit / 1000} seconds.`)
      )
    }, limit)
    const settle = (fn) => (value) => {
      clearTimeout(deadline)
      fn(value)
    }
    resolve = settle(resolve)
    reject = settle(reject)
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
        new Error(`Temps did not answer within ${limit / 1000} seconds.`)
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
/** The request a `notifications/cancelled` message cancels, if it is one. */
function cancelledRequestId(message) {
  if (!message || message.method !== 'notifications/cancelled') return undefined
  const params = message.params
  return params && typeof params === 'object' ? params.requestId : undefined
}

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

/** Folder deployments that may wait behind the one running. */
const MAX_WAITING_DEPLOYS = 4

/**
 * Messages the bridge works on at once. Past it the bridge stops reading:
 * the rest waits in the pipe, not in memory, and each forwarded call holds
 * its request and the server's answer only while it is one of these. A
 * cancellation is still read, so a call can always be withdrawn.
 */
const MAX_IN_FLIGHT = 16

/**
 * One task at a time, with at most `maxWaiting` waiting behind it. A folder
 * deployment holds its archive (up to the packing limit) and an upload
 * connection until the server answers, so running them one by one bounds
 * the bridge's memory to one of each; a call past the queue is refused at
 * once rather than held.
 *
 * `queue(task, token)` runs `task` in its turn. `queue.withdraw(token)`
 * takes a task that is still waiting out of the queue, freeing its place:
 * it never runs, and its promise resolves with `null`. A task already
 * running is not affected; it reports `false`.
 */
function serialQueue(maxWaiting) {
  const waiting = []
  let running = false
  const next = () => {
    if (running) return
    const entry = waiting.shift()
    if (!entry) return
    running = true
    Promise.resolve()
      .then(entry.task)
      .then(entry.resolve, entry.reject)
      .finally(() => {
        running = false
        next()
      })
  }
  const queue = (task, token) => {
    if (running && waiting.length >= maxWaiting) {
      return Promise.reject(
        new Error(
          `${waiting.length} folder deployments are already waiting for the one running. Try again once they finish.`
        )
      )
    }
    return new Promise((resolve, reject) => {
      waiting.push({ task, token, resolve, reject })
      next()
    })
  }
  queue.withdraw = (token) => {
    const at = waiting.findIndex((entry) => entry.token === token)
    if (at === -1) return false
    const [entry] = waiting.splice(at, 1)
    entry.resolve(null)
    return true
  }
  return queue
}

/**
 * Whether a line is an MCP cancellation, read even when the bridge is busy.
 * Only a notification: one with an `id` is a request, which is answered, so
 * it waits its turn like any other and cannot queue replies past the limit.
 */
function isCancellation(line) {
  if (!line.includes('notifications/cancelled')) return false
  try {
    const message = JSON.parse(line)
    return (
      message !== null &&
      typeof message === 'object' &&
      message.method === 'notifications/cancelled' &&
      !('id' in message)
    )
  } catch {
    return false
  }
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
  // Cancellations started past MAX_IN_FLIGHT while busy, themselves at most
  // MAX_IN_FLIGHT: a client sending nothing else cannot start them unbounded.
  let cancelling = 0
  let ended = false
  const deployOneAtATime = serialQueue(MAX_WAITING_DEPLOYS)
  // Folder deployments this bridge answers itself, by request id, so a
  // cancellation of one stays here: the server never saw the call.
  const localCalls = new Map()

  // A reply resolves once the app has taken it: while its reader lags, the
  // request stays in flight, so at most MAX_IN_FLIGHT replies wait in
  // memory and no further request is read meanwhile.
  // Every reply waiting for the same drain shares one set of listeners, so
  // a slow reader never adds more than three.
  let drained = null
  const write = (message) => {
    if (output.write(`${JSON.stringify(message)}\n`)) return Promise.resolve()
    if (output.destroyed || output.writableEnded) return Promise.resolve()
    if (!drained) {
      drained = new Promise((resolve) => {
        const done = () => {
          output.off('drain', done)
          output.off('close', done)
          output.off('error', done)
          drained = null
          resolve()
        }
        output.on('drain', done)
        output.on('close', done)
        output.on('error', done)
      })
    }
    return drained
  }
  const maybeExit = () => {
    if (ended && pending === 0 && !buffer.includes('\n')) {
      if (output !== process.stdout) output.end()
    }
  }

  const handle = async (line) => {
    let message
    try {
      message = JSON.parse(line)
    } catch {
      await write(
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
    const cancelled = cancelledRequestId(message)
    if (cancelled !== undefined && localCalls.has(cancelled)) {
      // MCP: the call is not answered once cancelled. One still waiting
      // leaves the queue, freeing its place; one preparing its archive
      // stops before its next request to Temps, the upload included; one
      // already uploading has sent its archive.
      const call = localCalls.get(cancelled)
      call.cancelled = true
      deployOneAtATime.withdraw(call)
      log(`temps: ${deploy.DEPLOY_TOOL_NAME} request ${cancelled} cancelled`)
      return
    }
    if (isRequest && isDeployCall(message)) {
      const call = { cancelled: false }
      localCalls.set(id, call)
      // Answered here: a tool error is a result Claude reads, not a failure.
      try {
        if (config instanceof Error) throw config
        const text = await deployOneAtATime(
          () =>
            deploy.deployFolder(
              config,
              message.params.arguments,
              (verb, path, headers, body) => {
                if (call.cancelled) {
                  return Promise.reject(
                    new Error(
                      `${deploy.DEPLOY_TOOL_NAME} request ${id} was cancelled; nothing more is sent to Temps.`
                    )
                  )
                }
                return send(config, verb, path, headers, body)
              }
            ),
          call
        )
        if (!call.cancelled) await write(toolResult(id, text, false))
      } catch (error) {
        const text = error instanceof Error ? error.message : String(error)
        if (!call.cancelled) await write(toolResult(id, text, true))
      } finally {
        localCalls.delete(id)
      }
      log(
        `temps: ${deploy.DEPLOY_TOOL_NAME} ${call.cancelled ? 'cancelled' : 'answered'} in ${Date.now() - started} ms`
      )
      return
    }
    try {
      if (config instanceof Error) throw config
      const replies = withDeployTool(
        message,
        await forward(config, message, session)
      )
      for (const reply of replies) await write(reply)
      log(`temps: ${method} answered in ${Date.now() - started} ms`)
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error)
      log(`temps: ${text}`)
      if (isRequest) await write(errorReply(id, -32000, text))
    }
  }

  // Start the messages read so far, up to MAX_IN_FLIGHT at once; reading
  // stops while that many are running and starts again as they finish.
  const start = (line, cancellation = false) => {
    pending++
    if (cancellation) cancelling++
    handle(line).finally(() => {
      pending--
      if (cancellation) cancelling--
      pump()
      maybeExit()
    })
  }
  const pump = () => {
    let newline
    while (pending < MAX_IN_FLIGHT && (newline = buffer.indexOf('\n')) !== -1) {
      const line = buffer.slice(0, newline).trim()
      buffer = buffer.slice(newline + 1)
      if (discarding) {
        discarding = false
        continue
      }
      if (line) start(line)
    }
    if (pending >= MAX_IN_FLIGHT && buffer.includes('\n')) {
      // Busy: take out the cancellations waiting behind, so a call can still
      // be withdrawn; the rest keep their order.
      const lines = buffer.split('\n')
      const partial = lines.pop()
      const kept = lines.filter((line, at) => {
        // The rest of a message too large to read is not a message.
        if (
          (discarding && at === 0) ||
          cancelling >= MAX_IN_FLIGHT ||
          !isCancellation(line.trim())
        ) {
          return true
        }
        start(line.trim(), true)
        return false
      })
      buffer = kept.length > 0 ? `${kept.join('\n')}\n${partial}` : partial
    }
    const full = buffer.includes('\n')
    if (full && !input.isPaused()) input.pause()
    if (!full && input.isPaused() && !ended) input.resume()
    if (!full && Buffer.byteLength(buffer) > MAX_MESSAGE_BYTES) {
      log(`temps: dropped a message larger than ${MAX_MESSAGE_BYTES} bytes`)
      buffer = ''
      discarding = true
    }
  }

  input.setEncoding('utf8')
  input.on('data', (chunk) => {
    buffer += chunk
    pump()
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
  serialQueue,
  MAX_WAITING_DEPLOYS,
  MAX_IN_FLIGHT,
  plainHttpAllowed,
  tailnetLocalAddresses,
  plainHttpSource,
  readConfig,
  parseSse,
  sendTo,
  statusError,
  run,
  main,
}
