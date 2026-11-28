// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Temps MCP bundle server.
 *
 * Claude Desktop starts this process from the bundle manifest and talks MCP
 * to it over stdio. The server:
 *
 * - always offers two local tools, `temps_setup_status` and
 *   `temps_install_guide`, so the extension explains itself and helps with
 *   installation before any Temps instance exists or is configured;
 * - once a console URL and API key are configured, forwards every other tool
 *   to that instance's `POST /mcp` endpoint (ADR-039) with a bearer key.
 *
 * It has no dependencies (global `fetch` only) and never runs commands on
 * the user's machine or any server: installation steps are returned as a
 * guide for the user or a shell-capable agent to run.
 */

export const SERVER_NAME = 'temps'
export const SERVER_VERSION = '0.1.0'

/** CLI package and dist-tag every guide uses. Kept equal to the skills' channel by a test. */
export const CLI_PACKAGE = '@temps-sdk/cli@beta'

export const SUPPORTED_PROTOCOL_VERSIONS = ['2025-06-18', '2025-03-26', '2024-11-05']
export const REQUEST_TIMEOUT_MS = 30_000

/** Console routes the onboarding output links to. */
export const CONSOLE_PATHS = {
  mcpServer: '/settings/mcp-server',
  apiKeys: '/settings/keys',
}

const LOOPBACK_HOSTS = new Set(['localhost', '127.0.0.1', '[::1]', '::1'])
const TRUE_VALUES = new Set(['1', 'true', 'yes', 'on'])

const SERVER_INSTRUCTIONS = [
  'Temps is a self-hosted deployment platform.',
  'Call temps_setup_status first when the user asks about Temps; it reports whether this extension is connected to an instance and exactly what to fix.',
  'Use temps_install_guide when the user wants to install Temps; present its steps and let the user run them.',
  'Never ask the user to paste an API key or password into the chat: the key is entered in the extension settings.',
].join(' ')

// ─── Configuration ───────────────────────────────────────────────────────────

/**
 * Treat empty values and unexpanded `${user_config.x}` placeholders (what a
 * host may pass when the user left a field blank) as unset.
 */
export function cleanValue(value) {
  if (typeof value !== 'string') return ''
  const trimmed = value.trim()
  if (trimmed.startsWith('${') && trimmed.endsWith('}')) return ''
  return trimmed
}

/**
 * Normalise a console URL to the server root, where `/mcp` is mounted.
 * Accepts a URL copied from the console or from a CLI context (which may end
 * in `/api`). Returns `{ baseUrl }` or `{ error }`.
 */
export function normalizeBaseUrl(raw) {
  const value = cleanValue(raw)
  if (!value) return { error: 'No Temps console URL is configured.' }

  let url
  try {
    url = new URL(value)
  } catch {
    return { error: `The configured Temps URL "${value}" is not a valid URL. Use the console address, e.g. https://temps.example.com.` }
  }
  if (url.protocol !== 'https:' && url.protocol !== 'http:') {
    return { error: `The configured Temps URL "${value}" must start with https:// (or http:// for a local instance).` }
  }
  if (url.username || url.password) {
    return { error: `The configured Temps URL for host ${url.host} must not contain credentials; enter the API key in its own field.` }
  }
  if (url.search || url.hash) {
    return { error: `The configured Temps URL "${url.origin}${url.pathname}" must not contain a query string or fragment.` }
  }

  let path = url.pathname.replace(/\/+$/, '')
  if (path.endsWith('/api')) path = path.slice(0, -'/api'.length)
  return { baseUrl: `${url.origin}${path}` }
}

export function isInsecureRemote(baseUrl) {
  const url = new URL(baseUrl)
  return url.protocol === 'http:' && !LOOPBACK_HOSTS.has(url.hostname)
}

export function readConfig(env) {
  const apiKey = cleanValue(env.TEMPS_API_KEY)
  const writeEnabled = TRUE_VALUES.has(cleanValue(env.TEMPS_MCP_WRITE).toLowerCase())
  const { baseUrl, error } = normalizeBaseUrl(env.TEMPS_URL)

  const problems = []
  if (error) problems.push(error)
  if (!apiKey) problems.push('No Temps API key is configured.')

  return {
    baseUrl: baseUrl ?? null,
    apiKey,
    writeEnabled,
    configured: problems.length === 0,
    problems,
  }
}

// ─── Remote Temps instance ───────────────────────────────────────────────────

export class RemoteError extends Error {
  /**
   * @param {'unreachable'|'unauthorized'|'forbidden'|'mcp_disabled'|'http'|'invalid_response'|'rpc'} kind
   * @param {string} message
   */
  constructor(kind, message) {
    super(message)
    this.name = 'RemoteError'
    this.kind = kind
  }
}

function describeFetchFailure(error) {
  if (error && typeof error === 'object') {
    if (error.name === 'TimeoutError' || error.name === 'AbortError') {
      return `no response within ${REQUEST_TIMEOUT_MS / 1000}s`
    }
    const cause = error.cause && typeof error.cause === 'object' ? error.cause : null
    if (cause && typeof cause.code === 'string') return cause.code
    if (typeof error.message === 'string') return error.message
  }
  return String(error)
}

async function problemDetail(res) {
  try {
    const body = await res.json()
    if (body && typeof body.detail === 'string') return body.detail
    if (body && typeof body.title === 'string') return body.title
  } catch {
    // Not a problem+json body; the status code is the useful part.
  }
  return null
}

export function createRemote(config, fetchImpl) {
  let nextId = 1

  async function call(method, params) {
    const url = `${config.baseUrl}/mcp${config.writeEnabled ? '?write=1' : ''}`
    let res
    try {
      res = await fetchImpl(url, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          accept: 'application/json',
          authorization: `Bearer ${config.apiKey}`,
        },
        body: JSON.stringify({ jsonrpc: '2.0', id: nextId++, method, params: params ?? {} }),
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      })
    } catch (error) {
      throw new RemoteError(
        'unreachable',
        `Could not reach Temps at ${config.baseUrl} for ${method}: ${describeFetchFailure(error)}.`,
      )
    }

    if (res.status === 401) {
      throw new RemoteError(
        'unauthorized',
        `Temps at ${config.baseUrl} rejected the API key for ${method} (401). Create a new key at ${config.baseUrl}${CONSOLE_PATHS.apiKeys} and update it in the extension settings.`,
      )
    }
    if (res.status === 403) {
      const detail = await problemDetail(res)
      throw new RemoteError(
        'forbidden',
        `The API key lacks permission for ${method} on ${config.baseUrl} (403)${detail ? `: ${detail}` : ''}. MCP needs at least read access to projects.`,
      )
    }
    if (res.status === 404) {
      throw new RemoteError(
        'mcp_disabled',
        `The MCP server is not enabled on ${config.baseUrl} (404 from /mcp). An admin can turn it on at ${config.baseUrl}${CONSOLE_PATHS.mcpServer}.`,
      )
    }
    if (!res.ok) {
      const detail = await problemDetail(res)
      throw new RemoteError(
        'http',
        `Temps at ${config.baseUrl} returned HTTP ${res.status} for ${method}${detail ? `: ${detail}` : ''}.`,
      )
    }

    let body
    try {
      body = await res.json()
    } catch (error) {
      throw new RemoteError(
        'invalid_response',
        `Temps at ${config.baseUrl} sent a response to ${method} that is not JSON: ${describeFetchFailure(error)}.`,
      )
    }
    if (body && body.error) {
      throw new RemoteError(
        'rpc',
        `Temps at ${config.baseUrl} rejected ${method}: ${body.error.message ?? 'unknown error'} (code ${body.error.code ?? 'unknown'}).`,
      )
    }
    return body ? body.result : undefined
  }

  /** Unauthenticated probe: `true` enabled, `false` disabled (404). Throws when unreachable. */
  async function probe() {
    let res
    try {
      res = await fetchImpl(`${config.baseUrl}/mcp/tools`, {
        headers: { accept: 'application/json' },
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      })
    } catch (error) {
      throw new RemoteError(
        'unreachable',
        `Could not reach Temps at ${config.baseUrl}: ${describeFetchFailure(error)}.`,
      )
    }
    if (res.status === 404) return false
    if (!res.ok) {
      throw new RemoteError('http', `Temps at ${config.baseUrl} returned HTTP ${res.status} for /mcp/tools.`)
    }
    return true
  }

  return { call, probe }
}

// ─── Local tools ─────────────────────────────────────────────────────────────

export const INSTALL_TARGETS = ['this_computer', 'existing_server', 'new_server']

export const LOCAL_TOOLS = [
  {
    name: 'temps_setup_status',
    title: 'Temps connection status',
    description:
      'Report whether this extension is connected to a Temps instance: the configured console URL, whether its MCP server is enabled, whether the API key works, and the exact next steps when something is missing. Call this first.',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false },
    annotations: { readOnlyHint: true, openWorldHint: true },
  },
  {
    name: 'temps_install_guide',
    title: 'How to install Temps',
    description:
      'Return step-by-step instructions for installing Temps on this computer, on an existing Linux server, or on a new server, followed by connecting this extension. Does not run anything: present the steps to the user.',
    inputSchema: {
      type: 'object',
      properties: {
        target: {
          type: 'string',
          enum: INSTALL_TARGETS,
          description:
            'this_computer: try Temps locally (needs Docker). existing_server: a Linux server the user can SSH into. new_server: the user still needs to create a server.',
        },
        has_domain: {
          type: 'boolean',
          description: 'Whether the user has a domain to point at the server. Without one, QuickStart uses an sslip.io address.',
        },
      },
      required: ['target'],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, openWorldHint: false },
  },
]

const LOCAL_TOOL_NAMES = new Set(LOCAL_TOOLS.map((tool) => tool.name))

function cli(args) {
  return `bunx ${CLI_PACKAGE} ${args}`
}

const CONNECT_STEPS = [
  '## Connect this extension',
  '',
  `1. Enable the MCP server once per instance: in the console open **Settings → MCP Server** (\`${CONSOLE_PATHS.mcpServer}\`), or run \`${cli('mcp enable')}\` as an admin.`,
  `2. Create an API key in **Settings → API Keys** (\`${CONSOLE_PATHS.apiKeys}\`). A read-only (reader) key is enough unless you want Claude to trigger deployments.`,
  '3. In Claude Desktop open **Settings → Extensions → Temps → Configure**, enter the console URL and the API key, and save. Keep the key out of the chat.',
  '4. Ask Claude to call `temps_setup_status` to confirm the connection.',
  '',
  'Using a coding agent (Claude Code, Codex, Cursor) instead? Run `' +
    cli('mcp add <client>') +
    '` there; it creates a scoped key and writes the client config for you.',
].join('\n')

const CLI_NOTE =
  `Commands use Bun's \`bunx\`; with Node.js only, replace \`bunx\` by \`npx\`. Keep the \`${CLI_PACKAGE}\` channel as written.`

function serverRequirements() {
  return [
    '- Linux on x86-64 or ARM64 with glibc 2.35+ (Ubuntu 22.04+, Debian 12+).',
    '- At least 2 CPU cores, 4 GB RAM and 20 GB disk (4 cores / 8 GB recommended for production).',
    '- SSH access with a key, root or passwordless sudo, and a host key you have verified.',
    '- Inbound TCP 22, 80 and 443 open for a public install.',
  ].join('\n')
}

function serverInstallSteps(hasDomain) {
  const domainStep = hasDomain
    ? '- **With your domain:** point its DNS at the server first. The installer\'s advanced mode asks for the domain and walks you through DNS and certificates; keep any DNS provider token in the installer prompt, not in a command line or the chat.'
    : '- **No domain yet:** QuickStart serves the console on an sslip.io address derived from the server IP, with HTTPS. You can add your own domain later.'

  return [
    '## Install on the server',
    '',
    domainStep,
    '',
    'From a terminal on your computer (needs Bun or Node.js), preview the plan, then run it:',
    '',
    '```bash',
    cli('setup --ssh user@your-server --email you@example.com --context production --dry-run'),
    cli('setup --ssh user@your-server --email you@example.com --context production'),
    '```',
    '',
    'It installs Temps over SSH, waits for it to become healthy and saves a CLI context named `production`.',
    '',
    'Or, logged in on the server itself, download the official installer, read it, then run it:',
    '',
    '```bash',
    'curl -fsSL https://temps.sh/deploy.sh -o deploy.sh',
    'less deploy.sh',
    'bash deploy.sh',
    '```',
    '',
    'The installer prints the console URL. Its log and `~/.temps/setup-result.json` on the server contain the first admin password: read them on the server, never paste them into a chat.',
  ].join('\n')
}

export function installGuide(args) {
  const target = args && typeof args.target === 'string' ? args.target : ''
  const hasDomain = Boolean(args && args.has_domain)

  if (!INSTALL_TARGETS.includes(target)) {
    throw new ToolInputError(
      `temps_install_guide: "target" must be one of ${INSTALL_TARGETS.join(', ')}; got ${JSON.stringify(args ? args.target : undefined)}.`,
    )
  }

  if (target === 'this_computer') {
    return [
      '# Run Temps on this computer',
      '',
      'Best for trying Temps: apps you deploy are reachable only from this machine.',
      '',
      '## Requirements',
      '',
      '- macOS or Linux on x86-64 or ARM64.',
      '- Docker running (Docker Desktop, OrbStack or Colima).',
      '- Bun or Node.js.',
      '',
      '## Install',
      '',
      '```bash',
      cli('start --email you@example.com'),
      '```',
      '',
      'Use your own email: it is your admin sign-in for the console.',
      '',
      'It downloads the Temps server, starts its database in Docker, sets up the platform, starts it in the background, signs the CLI in and prints the console URL (http://localhost:8080 unless that port is taken). Run it again to restart Temps; `' +
        cli('stop') +
        '` stops it.',
      '',
      CLI_NOTE,
      '',
      CONNECT_STEPS,
    ].join('\n')
  }

  const intro =
    target === 'new_server'
      ? [
          '# Install Temps on a new server',
          '',
          '## Create the server',
          '',
          'Create a VM at the provider of your choice (this is billed by that provider):',
          '',
          '- Ubuntu 24.04 LTS, at least 2 vCPU and 4 GB RAM.',
          '- Add your SSH public key while creating it.',
          '- Allow inbound TCP 22, 80 and 443 in its firewall.',
          '',
          'Verify the host key fingerprint from the provider\'s console the first time you SSH in, then continue below.',
          '',
          '## Requirements',
          '',
          serverRequirements(),
        ]
      : ['# Install Temps on your server', '', '## Requirements', '', serverRequirements()]

  return [...intro, '', serverInstallSteps(hasDomain), '', CLI_NOTE, '', CONNECT_STEPS].join('\n')
}

export class ToolInputError extends Error {
  constructor(message) {
    super(message)
    this.name = 'ToolInputError'
  }
}

export async function setupStatus(config, remote) {
  const status = {
    configured: config.configured,
    temps_url: config.baseUrl,
    write_tools_enabled: config.writeEnabled,
    mcp_server: 'unknown',
    authenticated: null,
    remote_tools: [],
    problems: [...config.problems],
    next_steps: [],
  }

  if (config.baseUrl && isInsecureRemote(config.baseUrl)) {
    status.problems.push(
      `${config.baseUrl} uses plain HTTP on a non-local host, so the API key travels unencrypted. Use the https:// console address.`,
    )
  }

  if (!config.baseUrl) {
    status.next_steps.push(
      'No Temps instance yet? Call temps_install_guide with the place the user wants to run it.',
      'Already have one? In Claude Desktop open Settings → Extensions → Temps → Configure and enter the console URL and an API key.',
    )
    return status
  }

  try {
    const enabled = await remote.probe()
    status.mcp_server = enabled ? 'enabled' : 'disabled'
    if (!enabled) {
      status.problems.push(`The MCP server is not enabled on ${config.baseUrl}.`)
      status.next_steps.push(
        `Enable it at ${config.baseUrl}${CONSOLE_PATHS.mcpServer} (admin), or run \`${cli(`mcp enable --url ${config.baseUrl}`)}\`.`,
      )
    }
  } catch (error) {
    status.mcp_server = 'unreachable'
    status.problems.push(error instanceof Error ? error.message : String(error))
    status.next_steps.push('Check the console URL in the extension settings and that the instance is running.')
    return status
  }

  if (!config.apiKey) {
    status.next_steps.push(
      `Create an API key at ${config.baseUrl}${CONSOLE_PATHS.apiKeys} and enter it in the extension settings.`,
    )
    return status
  }
  if (status.mcp_server !== 'enabled') return status

  try {
    const result = await remote.call('tools/list', {})
    status.authenticated = true
    status.remote_tools = Array.isArray(result && result.tools) ? result.tools.map((tool) => tool.name) : []
  } catch (error) {
    status.authenticated = error instanceof RemoteError && error.kind === 'unauthorized' ? false : null
    status.problems.push(error instanceof Error ? error.message : String(error))
  }

  if (status.authenticated && !config.writeEnabled) {
    status.next_steps.push(
      'Read-only mode. To let Claude propose deployments (each still needs confirmation), enable "Allow write tools" in the extension settings and use a key with deployment access.',
    )
  }
  return status
}

// ─── JSON-RPC dispatch ───────────────────────────────────────────────────────

const RPC_METHOD_NOT_FOUND = -32601
const RPC_INVALID_PARAMS = -32602
const RPC_PARSE_ERROR = -32700
const RPC_INVALID_REQUEST = -32600

function rpcResult(id, result) {
  return { jsonrpc: '2.0', id, result }
}

function rpcError(id, code, message) {
  return { jsonrpc: '2.0', id, error: { code, message } }
}

function textResult(text, isError = false) {
  return isError ? { content: [{ type: 'text', text }], isError: true } : { content: [{ type: 'text', text }] }
}

/**
 * @param {{ env: Record<string, string | undefined>, fetchImpl?: typeof fetch, log?: (entry: object) => void }} options
 */
export function createServer({ env, fetchImpl = globalThis.fetch, log = () => {} }) {
  const config = readConfig(env)
  const remote = config.baseUrl ? createRemote(config, fetchImpl) : null

  log({
    level: 'info',
    msg: 'temps mcp bundle starting',
    version: SERVER_VERSION,
    configured: config.configured,
    temps_url: config.baseUrl,
    write_tools_enabled: config.writeEnabled,
  })

  async function listTools() {
    if (!config.configured || !remote) return { tools: LOCAL_TOOLS }
    try {
      const result = await remote.call('tools/list', {})
      const remoteTools = Array.isArray(result && result.tools)
        ? result.tools.filter((tool) => tool && typeof tool.name === 'string' && !LOCAL_TOOL_NAMES.has(tool.name))
        : []
      return { tools: [...LOCAL_TOOLS, ...remoteTools] }
    } catch (error) {
      // Degrade to the local tools: temps_setup_status explains the failure.
      log({
        level: 'warn',
        msg: 'tools/list against Temps failed; offering local tools only',
        temps_url: config.baseUrl,
        error: error instanceof Error ? error.message : String(error),
      })
      return { tools: LOCAL_TOOLS }
    }
  }

  async function callTool(params) {
    const name = params && typeof params.name === 'string' ? params.name : ''
    const args = params && params.arguments && typeof params.arguments === 'object' ? params.arguments : {}

    if (name === 'temps_setup_status') {
      const status = await setupStatus(config, remote)
      return textResult(JSON.stringify(status, null, 2))
    }
    if (name === 'temps_install_guide') {
      try {
        return textResult(installGuide(args))
      } catch (error) {
        if (error instanceof ToolInputError) return textResult(error.message, true)
        throw error
      }
    }

    if (!config.configured || !remote) {
      return textResult(
        `Cannot run Temps tool "${name}": this extension is not connected to a Temps instance (${config.problems.join(' ')}) Call temps_setup_status for the steps to connect it.`,
        true,
      )
    }

    try {
      return await remote.call('tools/call', { name, arguments: args })
    } catch (error) {
      log({
        level: 'warn',
        msg: 'tools/call against Temps failed',
        tool: name,
        temps_url: config.baseUrl,
        error: error instanceof Error ? error.message : String(error),
      })
      return textResult(
        `Temps tool "${name}" failed: ${error instanceof Error ? error.message : String(error)}`,
        true,
      )
    }
  }

  /**
   * Handle one parsed JSON-RPC message. Resolves to the response, or `null`
   * for notifications.
   */
  async function handle(message) {
    if (!message || typeof message !== 'object' || Array.isArray(message) || message.jsonrpc !== '2.0') {
      return rpcError(null, RPC_INVALID_REQUEST, 'Invalid JSON-RPC 2.0 request: expected an object with "jsonrpc": "2.0".')
    }
    const isNotification = !('id' in message)
    const id = isNotification ? null : message.id
    if (typeof message.method !== 'string') {
      return isNotification ? null : rpcError(id, RPC_INVALID_REQUEST, 'Invalid JSON-RPC request: "method" must be a string.')
    }
    if (isNotification) return null

    switch (message.method) {
      case 'initialize': {
        const requested = message.params && message.params.protocolVersion
        const protocolVersion = SUPPORTED_PROTOCOL_VERSIONS.includes(requested)
          ? requested
          : SUPPORTED_PROTOCOL_VERSIONS[0]
        return rpcResult(id, {
          protocolVersion,
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
          instructions: SERVER_INSTRUCTIONS,
        })
      }
      case 'ping':
        return rpcResult(id, {})
      case 'tools/list':
        return rpcResult(id, await listTools())
      case 'tools/call': {
        if (!message.params || typeof message.params.name !== 'string') {
          return rpcError(id, RPC_INVALID_PARAMS, 'tools/call requires params.name (string).')
        }
        return rpcResult(id, await callTool(message.params))
      }
      default:
        return rpcError(id, RPC_METHOD_NOT_FOUND, `Method "${message.method}" is not supported by the Temps MCP bundle.`)
    }
  }

  /** Handle one raw line from stdin. */
  async function handleLine(line) {
    const trimmed = line.trim()
    if (!trimmed) return null
    let message
    try {
      message = JSON.parse(trimmed)
    } catch (error) {
      return rpcError(null, RPC_PARSE_ERROR, `Could not parse JSON-RPC message: ${error instanceof Error ? error.message : String(error)}`)
    }
    try {
      return await handle(message)
    } catch (error) {
      log({
        level: 'error',
        msg: 'unhandled error while handling MCP message',
        method: message && message.method,
        error: error instanceof Error ? error.message : String(error),
      })
      return rpcError(
        message && 'id' in message ? message.id : null,
        -32603,
        `Internal error in the Temps MCP bundle while handling ${message && message.method}: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
  }

  return { config, handle, handleLine }
}
