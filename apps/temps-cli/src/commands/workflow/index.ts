import type { Command } from 'commander'
import { requireAuth, credentials, config } from '../../config/store.js'
import { normalizeApiUrl } from '../../lib/api-client.js'
import { setupClient, client } from '../../lib/api-client.js'
import { getProjectBySlug } from '../../api/sdk.gen.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import {
  newline,
  header,
  icons,
  json as jsonOut,
  colors,
  success,
  info,
  warning,
  error as errorOut,
  keyValue,
} from '../../ui/output.js'

// ── Types mirroring server responses ────────────────────────────────────────

interface AgentRunResponse {
  id: number
  project_id: number
  config_id: number | null
  agent_slug: string | null
  agent_name: string | null
  source: string
  ephemeral_yaml: string | null
  status: string
  trigger_type: string
  pr_url: string | null
  pr_number: number | null
  preview_url: string | null
  error_message: string | null
  ai_output: string | null
  ai_model: string | null
  ai_provider: string | null
  tokens_input: number
  tokens_output: number
  estimated_cost_cents: number
  files_changed: number
  analysis: string | null
  started_at: string | null
  completed_at: string | null
  created_at: string
  sandbox_enabled: boolean
}

interface AgentConfigResponse {
  id: number
  slug: string
  name: string
  description: string | null
  enabled: boolean
  ai_provider: string
  ai_model: string | null
  deliverable: string
  trigger_config: unknown
}

interface ListAgentsResponse {
  items: AgentConfigResponse[]
  total: number
}

// ── HTTP plumbing ───────────────────────────────────────────────────────────

interface AgentApi {
  baseUrl: string
  apiKey: string
}

async function authClient(): Promise<AgentApi> {
  await requireAuth()
  const apiKey = await credentials.getApiKey()
  if (!apiKey) {
    throw new Error('Not authenticated. Run `temps login` first.')
  }
  const baseUrl = normalizeApiUrl(config.get('apiUrl'))
  return { baseUrl, apiKey }
}

async function apiRequest<T>(
  api: AgentApi,
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('Authorization', `Bearer ${api.apiKey}`)
  if (init.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }

  const response = await fetch(`${api.baseUrl}${path}`, { ...init, headers })

  if (!response.ok) {
    throw await readApiError(response)
  }

  if (response.status === 204) {
    return undefined as T
  }

  return (await response.json()) as T
}

export async function readApiError(response: Response): Promise<Error> {
  const text = await response.text().catch(() => '')
  try {
    const problem = JSON.parse(text) as { title?: string; detail?: string }
    const title = problem.title ?? `HTTP ${response.status}`
    const detail = problem.detail ? ` — ${problem.detail}` : ''
    return new Error(`${title}${detail}`)
  } catch {
    return new Error(`HTTP ${response.status}: ${text || response.statusText}`)
  }
}

// `slug` may contain dots ("owner.repo") or other URL-significant chars; the
// server treats the slug as an opaque identifier so encode it before splicing.
function encodeSlug(slug: string): string {
  return encodeURIComponent(slug)
}

// Resolve project slug → numeric id via the typed SDK. We need the id because
// every agent endpoint is keyed on `project_id`.
async function resolveProjectId(projectFlag: string | undefined): Promise<{
  id: number
  slug: string
  source: string
}> {
  await setupClient()
  const resolved = await requireProjectSlug(projectFlag)
  const { data, error } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return { id: data.id, slug: resolved.slug, source: resolved.source }
}

export function statusColor(status: string): string {
  const s = status.toLowerCase()
  if (s === 'completed') return colors.success(status)
  if (s === 'failed' || s === 'cancelled' || s === 'no_fix') {
    return colors.error(status)
  }
  if (s === 'running' || s === 'pending') return colors.warning(status)
  return status
}

export function levelColor(level: string): string {
  const l = level.toLowerCase()
  if (l === 'error') return colors.error(level.padEnd(5))
  if (l === 'warn' || l === 'warning') return colors.warning(level.padEnd(5))
  if (l === 'success') return colors.success(level.padEnd(5))
  return colors.muted(level.padEnd(5))
}

// ── Commander wiring ────────────────────────────────────────────────────────

export function registerWorkflowCommands(program: Command): void {
  const workflow = program
    .command('workflow')
    .alias('wf')
    .description('Trigger and inspect agent/workflow runs')

  workflow
    .command('list')
    .alias('ls')
    .description('List workflows/agents available on this project')
    .option('-p, --project <slug>', 'Project slug (auto-detect from .temps/config.json)')
    .option('--json', 'Output as JSON')
    .action(listAction)

  workflow
    .command('run [slug]')
    .description('Trigger a workflow and stream its output')
    .option('-p, --project <slug>', 'Project slug (auto-detect from .temps/config.json)')
    .option(
      '-c, --context <text>',
      'Free-form user context passed to the workflow (e.g. a bug description)',
    )
    .option(
      '-f, --from-file <path>',
      'Run an ephemeral workflow from a local YAML file (no server-side persistence). ' +
        'Mutually exclusive with <slug>.',
    )
    .option(
      '-e, --error-group <id>',
      'Link this run to an error group id. The workflow will see the error type, ' +
        'message, and stack trace via the usual {{error_type}} / {{error_message}} ' +
        'template fields. Works with both committed slugs and --from-file.',
    )
    .option(
      '--cpu <cores>',
      'CPU cores for the ephemeral sandbox (0.1–4.0). Overrides the YAML value. ' +
        'Only applies with --from-file.',
    )
    .option(
      '--memory <mb>',
      'Memory limit in MB for the ephemeral sandbox (128–8192). Overrides the YAML value. ' +
        'Only applies with --from-file.',
    )
    .option('--no-follow', 'Return immediately after queueing instead of streaming logs')
    .option('--json', 'Print the run record as JSON when it terminates')
    .action(runAction)
}

// ── Actions ─────────────────────────────────────────────────────────────────

interface ListOptions {
  project?: string
  json?: boolean
}

async function listAction(options: ListOptions): Promise<void> {
  const project = await resolveProjectId(options.project)
  if (project.source !== 'flag') {
    info(`Using project ${colors.bold(project.slug)} (from ${project.source})`)
  }

  const api = await authClient()
  const data = await withSpinner('Fetching workflows...', () =>
    apiRequest<ListAgentsResponse>(api, `/projects/${project.id}/agents`),
  )

  if (options.json) {
    jsonOut(data)
    return
  }

  newline()
  header(`${icons.info} Workflows for ${project.slug} (${data.total})`)

  if (data.items.length === 0) {
    info('No workflows found. Add one to .temps/workflows/<slug>.yaml and redeploy.')
    newline()
    return
  }

  const columns: TableColumn<AgentConfigResponse>[] = [
    { header: 'Slug', key: 'slug', color: (v) => colors.primary(v) },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    {
      header: 'Enabled',
      accessor: (a) => (a.enabled ? 'yes' : 'no'),
      color: (v) => (v === 'yes' ? colors.success(v) : colors.muted(v)),
    },
    { header: 'Provider', key: 'ai_provider', color: (v) => colors.muted(v) },
    {
      header: 'Deliverable',
      key: 'deliverable',
      color: (v) => colors.muted(v),
    },
  ]

  printTable(data.items, columns, { style: 'minimal' })
  newline()
}

interface RunOptions {
  project?: string
  context?: string
  fromFile?: string
  errorGroup?: string
  cpu?: string
  memory?: string
  follow?: boolean // commander sets `false` for --no-follow
  json?: boolean
}

/**
 * The three argument-combination rules for `workflow run`: <slug> and
 * --from-file are mutually exclusive, one of them is required, and
 * --cpu/--memory only make sense for an ephemeral (--from-file) run.
 * Returns the message to print, or null when the combination is valid.
 */
export function validateRunArgs(
  slug: string | undefined,
  options: Pick<RunOptions, 'fromFile' | 'cpu' | 'memory'>
): string | null {
  if (slug && options.fromFile) {
    return 'Pass either <slug> or --from-file, not both.'
  }
  if (!slug && !options.fromFile) {
    return 'Specify a workflow slug, or use --from-file <path> for an ephemeral run.'
  }
  if (!options.fromFile && (options.cpu || options.memory)) {
    return '--cpu / --memory only apply to ephemeral runs (use --from-file).'
  }
  return null
}

/** Throws with the exact CLI-facing message when --error-group isn't a positive integer. */
export function parseErrorGroupId(raw: string | undefined): number | undefined {
  if (!raw) return undefined
  const parsed = Number(raw)
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`Invalid --error-group value (must be a positive integer): ${raw}`)
  }
  return parsed
}

/** Throws with the exact CLI-facing message when --cpu isn't a positive number. */
export function parseCpuLimit(raw: string | undefined): number | undefined {
  if (!raw) return undefined
  const cpu = Number(raw)
  if (!Number.isFinite(cpu) || cpu <= 0) {
    throw new Error(`Invalid --cpu value: ${raw}`)
  }
  return cpu
}

/** Throws with the exact CLI-facing message when --memory isn't a positive integer. */
export function parseMemoryLimitMb(raw: string | undefined): number | undefined {
  if (!raw) return undefined
  const mem = Number(raw)
  if (!Number.isInteger(mem) || mem <= 0) {
    throw new Error(`Invalid --memory value (must be a positive integer MB): ${raw}`)
  }
  return mem
}

async function runAction(slug: string | undefined, options: RunOptions): Promise<void> {
  // Two mutually-exclusive modes:
  //   1. `temps wf run <slug>`              — trigger a workflow already
  //                                            committed to .temps/workflows/.
  //   2. `temps wf run --from-file foo.yaml` — ephemeral dry-run; the YAML is
  //                                            POSTed once and never persists
  //                                            in project_agents server-side.
  const argError = validateRunArgs(slug, options)
  if (argError) {
    errorOut(argError)
    process.exitCode = 2
    return
  }
  let errorGroupId: number | undefined
  try {
    errorGroupId = parseErrorGroupId(options.errorGroup)
  } catch (err) {
    errorOut((err as Error).message)
    process.exitCode = 2
    return
  }

  const project = await resolveProjectId(options.project)
  if (project.source !== 'flag') {
    info(`Using project ${colors.bold(project.slug)} (from ${project.source})`)
  }

  const api = await authClient()

  let run: AgentRunResponse
  let runLabel: string

  if (options.fromFile) {
    // Read the YAML up front so the user gets a fast local error if the path
    // is wrong — much better DX than letting the server reject an empty body.
    const file = Bun.file(options.fromFile)
    if (!(await file.exists())) {
      errorOut(`File not found: ${options.fromFile}`)
      process.exitCode = 2
      return
    }
    const yaml = await file.text()
    if (!yaml.trim()) {
      errorOut(`File is empty: ${options.fromFile}`)
      process.exitCode = 2
      return
    }

    const body: Record<string, unknown> = { yaml }
    if (options.context) body.user_context = options.context
    try {
      const cpu = parseCpuLimit(options.cpu)
      if (cpu !== undefined) body.cpu_limit = cpu
      const memory = parseMemoryLimitMb(options.memory)
      if (memory !== undefined) body.memory_limit_mb = memory
    } catch (err) {
      errorOut((err as Error).message)
      process.exitCode = 2
      return
    }
    if (errorGroupId !== undefined) {
      body.error_group_id = errorGroupId
    }

    runLabel =
      errorGroupId !== undefined
        ? `ephemeral workflow from ${options.fromFile} for error group ${errorGroupId}`
        : `ephemeral workflow from ${options.fromFile}`
    run = await withSpinner(`Triggering ${runLabel}...`, () =>
      apiRequest<AgentRunResponse>(api, `/projects/${project.id}/workflows/dry-run`, {
        method: 'POST',
        body: JSON.stringify(body),
      }),
    )
    warning(
      'Ephemeral run — workflow YAML is not stored in project_agents. ' +
        'Deliverable forced to "report".',
    )
  } else {
    const body: Record<string, unknown> = {}
    if (options.context) body.user_context = options.context
    if (errorGroupId !== undefined) {
      body.trigger_source_type = 'error_group'
      body.trigger_source_id = errorGroupId
    }

    runLabel =
      errorGroupId !== undefined
        ? `workflow "${slug}" for error group ${errorGroupId}`
        : `workflow "${slug}"`
    run = await withSpinner(`Triggering ${runLabel}...`, () =>
      apiRequest<AgentRunResponse>(
        api,
        `/projects/${project.id}/agents/${encodeSlug(slug as string)}/trigger`,
        { method: 'POST', body: JSON.stringify(body) },
      ),
    )
  }

  success(`Run #${run.id} queued (status: ${statusColor(run.status)})`)
  info('Executing inside Docker sandbox')

  if (options.follow === false) {
    info(
      `Inspect later with: temps workflow inspect ${run.id} --project ${project.slug}`,
    )
    return
  }

  newline()
  header(`${icons.info} Streaming run #${run.id}`)
  await streamLogs(api, project.id, run.id)

  // Fetch final run state to surface PR URL, error, and exit code.
  const final = await apiRequest<{ run: AgentRunResponse }>(
    api,
    `/projects/${project.id}/agents/runs/${run.id}`,
  )

  newline()
  printRunSummary(final.run)

  if (options.json) {
    newline()
    jsonOut(final.run)
  }

  // Exit code mirrors the run's terminal status so this command can be used
  // in CI / shell pipelines.
  if (final.run.status !== 'completed') {
    process.exitCode = 1
  }
}

// ── SSE streaming ──────────────────────────────────────────────────────────

interface LogPayload {
  id: number
  level: string
  message: string
  metadata?: unknown
  created_at: string
}

interface StatusPayload {
  type: 'run_status'
  status: string
}

/**
 * Consume the server's SSE stream of agent_run_logs. The server polls the
 * logs table every 500ms and emits one `data:` frame per new log row, plus
 * a final `event: status` frame when the run reaches a terminal state. We
 * print logs as they arrive and exit when we see the status frame (or when
 * the connection closes — the server closes after the status frame).
 */
async function streamLogs(
  api: AgentApi,
  projectId: number,
  runId: number,
): Promise<void> {
  const url = `${api.baseUrl}/projects/${projectId}/agents/runs/${runId}/stream`
  const response = await fetch(url, {
    headers: {
      Authorization: `Bearer ${api.apiKey}`,
      Accept: 'text/event-stream',
    },
  })

  if (!response.ok) {
    throw await readApiError(response)
  }
  if (!response.body) {
    throw new Error('Server returned no response body for log stream')
  }

  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  let eventName = ''

  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })

      let idx: number
      while ((idx = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, idx).replace(/\r$/, '')
        buffer = buffer.slice(idx + 1)

        if (line === '') {
          eventName = ''
          continue
        }

        if (line.startsWith(':')) {
          // Heartbeat comment frame from the server's KeepAlive — ignore.
          continue
        }

        if (line.startsWith('event:')) {
          eventName = line.slice(6).trim()
          continue
        }

        if (!line.startsWith('data:')) continue
        const data = line.slice(5).replace(/^\s/, '')

        if (eventName === 'status') {
          // Final terminal-status frame; the server closes after this.
          try {
            const payload = JSON.parse(data) as StatusPayload
            console.log(
              `${icons.arrow} Run reached terminal status: ${statusColor(payload.status)}`,
            )
          } catch {
            // ignore malformed status payload
          }
          return
        }

        // Default (no event:) frames carry log entries.
        try {
          const payload = JSON.parse(data) as LogPayload
          renderLogEntry(payload)
        } catch {
          process.stdout.write(data + '\n')
        }
      }
    }
  } finally {
    reader.releaseLock()
  }
}

function formatTime(isoString: string): string {
  try {
    const d = new Date(isoString)
    return d.toLocaleTimeString('en-US', { hour12: false })
  } catch {
    return isoString
  }
}

// ── ai_event rendering ─────────────────────────────────────────────────────
//
// The server forwards raw CLI stream events (Claude stream-json, Codex item
// envelopes, OpenCode message.part frames) verbatim as `level=ai_event` logs.
// We mirror the web UI's `extractToolEvent` + `parseOpenCodeOutput` rendering
// so the terminal shows a readable timeline instead of raw JSON blobs.

interface ToolEvent {
  kind: 'call' | 'result' | 'thinking' | 'text' | 'step' | 'step_end'
  toolUseId: string
  name?: string
  summary: string
  detail?: string
  isError?: boolean
}

const TRUNC = 160

export function truncate(s: string, max = TRUNC): string {
  const t = s.replace(/\s+/g, ' ').trim()
  return t.length > max ? t.slice(0, max) + '…' : t
}

export function summarizeToolInput(
  name: string,
  input: Record<string, unknown> | undefined,
): string {
  if (!input) return ''
  // Normalize to lower-case for matching — Claude uses "Read"/"Bash",
  // opencode uses "read"/"bash", Codex uses "command"/"file" etc. We
  // capitalize for display elsewhere; here we key off semantics.
  const key = name.toLowerCase()
  switch (key) {
    case 'read':
      return (input.file_path as string) ?? (input.path as string) ?? ''
    case 'write':
      return (input.file_path as string) ?? (input.path as string) ?? ''
    case 'edit':
      return (input.file_path as string) ?? (input.path as string) ?? ''
    case 'bash':
    case 'command': {
      const cmd = (input.command as string) ?? ''
      return `$ ${truncate(cmd, 120)}`
    }
    case 'glob':
      return (input.pattern as string) ?? ''
    case 'grep':
      return (input.pattern as string) ?? ''
    case 'skill':
      return (input.skill as string) ?? ''
    case 'webfetch':
    case 'fetch':
      return (input.url as string) ?? ''
    case 'websearch':
    case 'search':
      return (input.query as string) ?? ''
    case 'task':
      return truncate((input.prompt as string) ?? (input.description as string) ?? '', 120)
    case 'todowrite':
      return 'todos'
    default: {
      // Fallback: show the first short string value so the user sees
      // *something* instead of a bare tool name.
      for (const v of Object.values(input)) {
        if (typeof v === 'string' && v.length > 0 && v.length < 200) {
          return truncate(v, 120)
        }
      }
      return ''
    }
  }
}

export function extractAiEvent(message: string): ToolEvent | null {
  let obj: Record<string, unknown>
  try {
    obj = JSON.parse(message)
  } catch {
    return null
  }
  const type = obj.type as string | undefined

  // Claude stream-json: assistant messages carry tool_use or thinking blocks.
  if (type === 'assistant') {
    const msg = obj.message as { content?: unknown[] } | undefined
    const blocks = msg?.content
    if (!Array.isArray(blocks)) return null
    for (const block of blocks) {
      const b = block as Record<string, unknown>
      if (b.type === 'tool_use') {
        const name = (b.name as string) ?? 'tool'
        const input = b.input as Record<string, unknown> | undefined
        return {
          kind: 'call',
          toolUseId: (b.id as string) ?? '',
          name,
          summary: summarizeToolInput(name, input),
        }
      }
      if (b.type === 'thinking') {
        const text = (b.thinking as string) ?? ''
        if (!text.trim()) continue
        return { kind: 'thinking', toolUseId: '', summary: text }
      }
      if (b.type === 'text') {
        const text = (b.text as string) ?? ''
        if (!text.trim()) continue
        return { kind: 'text', toolUseId: '', summary: text }
      }
    }
    return null
  }

  if (type === 'user') {
    const msg = obj.message as { content?: unknown[] } | undefined
    const blocks = msg?.content
    if (!Array.isArray(blocks)) return null
    for (const block of blocks) {
      const b = block as Record<string, unknown>
      if (b.type === 'tool_result') {
        const content = b.content
        let text = ''
        if (typeof content === 'string') text = content
        else if (Array.isArray(content)) {
          text = content
            .map((c) => {
              const cc = c as Record<string, unknown>
              if (typeof cc.text === 'string') return cc.text
              if (typeof cc.content === 'string') return cc.content
              return ''
            })
            .join('\n')
        }
        return {
          kind: 'result',
          toolUseId: (b.tool_use_id as string) ?? '',
          summary: truncate(text),
          isError: b.is_error === true,
        }
      }
    }
    return null
  }

  // Codex: item envelopes (command_execution, mcp_tool_call, file_change,
  // web_search, reasoning).
  if (type === 'item.started' || type === 'item.completed') {
    const item = obj.item as Record<string, unknown> | undefined
    if (!item) return null
    const itemType = item.type as string | undefined
    if (itemType === 'reasoning') {
      const text = (item.text as string) ?? ''
      if (!text.trim()) return null
      return { kind: 'thinking', toolUseId: '', summary: text }
    }
    const id = (item.id as string) ?? ''
    if (itemType === 'command_execution') {
      if (type === 'item.started') {
        const cmd = (item.command as string) ?? ''
        return {
          kind: 'call',
          toolUseId: id,
          name: 'command',
          summary: `$ ${truncate(cmd, 120)}`,
        }
      }
      const exit = item.exit_code ?? item.status
      const stdout = (item.stdout as string) ?? ''
      const stderr = (item.stderr as string) ?? ''
      return {
        kind: 'result',
        toolUseId: id,
        summary:
          truncate(stdout) || truncate(stderr) || `exit ${exit ?? '?'}`,
        isError: typeof exit === 'number' && exit !== 0,
      }
    }
    if (itemType === 'mcp_tool_call') {
      if (type === 'item.started') {
        const server = (item.server as string) ?? ''
        const tool = (item.tool as string) ?? ''
        return {
          kind: 'call',
          toolUseId: id,
          name: tool,
          summary: server ? `${server}::${tool}` : tool,
        }
      }
      const result = item.result ?? item.output
      const resultStr =
        typeof result === 'string' ? result : JSON.stringify(result ?? '')
      return {
        kind: 'result',
        toolUseId: id,
        summary: truncate(resultStr),
        isError: (item.is_error as boolean) === true,
      }
    }
    if (itemType === 'file_change') {
      if (type === 'item.started') {
        const path = (item.path as string) ?? ''
        const op = (item.operation as string) ?? 'edit'
        return { kind: 'call', toolUseId: id, name: 'file', summary: `${op} ${path}` }
      }
      return {
        kind: 'result',
        toolUseId: id,
        summary: `ok ${(item.path as string) ?? ''}`,
      }
    }
    if (itemType === 'web_search') {
      if (type === 'item.started') {
        return {
          kind: 'call',
          toolUseId: id,
          name: 'web_search',
          summary: `Search ${(item.query as string) ?? ''}`,
        }
      }
      const results = item.results
      return {
        kind: 'result',
        toolUseId: id,
        summary: Array.isArray(results) ? `${results.length} result(s)` : 'done',
      }
    }
    return null
  }

  // OpenCode: top-level `reasoning`, `tool`, `step_start`, `step_finish`,
  // `text`, and `message.part.updated` events.
  if (type === 'reasoning') {
    const part = obj.part as Record<string, unknown> | undefined
    const text = (part?.text as string) ?? ''
    if (!text.trim()) return null
    return { kind: 'thinking', toolUseId: '', summary: text }
  }

  if (type === 'tool') {
    const part = obj.part as Record<string, unknown> | undefined
    if (!part) return null
    // OpenCode field names: `name` for the tool, `id` for the call, `state`
    // is a plain string ("pending" | "running" | "completed" | "error"),
    // and input/output/result live on the part itself (not nested under
    // state). Mirrors `parseOpenCodeOutput` in AutopilotRunDetail.tsx.
    const rawName = (part.name as string) ?? (part.tool as string) ?? 'tool'
    const tool = rawName.charAt(0).toUpperCase() + rawName.slice(1)
    const callId = (part.id as string) ?? (part.callID as string) ?? ''
    const state = part.state as string | undefined
    const parsedArgs = (() => {
      if (typeof part.args === 'string') {
        try {
          return JSON.parse(part.args as string) as Record<string, unknown>
        } catch {
          return undefined
        }
      }
      return undefined
    })()
    const input =
      (part.input as Record<string, unknown> | undefined) ?? parsedArgs
    if (state === 'running' || state === 'pending') {
      return {
        kind: 'call',
        toolUseId: callId,
        name: tool,
        summary: summarizeToolInput(tool, input),
      }
    }
    if (state === 'completed' || state === 'error') {
      const output = part.result ?? part.output
      const outStr =
        typeof output === 'string' ? output : JSON.stringify(output ?? '')
      return {
        kind: 'result',
        toolUseId: callId,
        summary: truncate(outStr) || (state === 'error' ? '(error)' : '(empty)'),
        isError: state === 'error',
      }
    }
    return null
  }

  if (type === 'text') {
    const part = obj.part as Record<string, unknown> | undefined
    const text = (part?.text as string) ?? ''
    if (!text.trim()) return null
    return { kind: 'text', toolUseId: '', summary: text }
  }

  if (type === 'step_start') {
    // Noisy — emit a dim divider so the user sees turn boundaries without
    // the raw JSON.
    return { kind: 'step', toolUseId: '', summary: '' }
  }

  if (type === 'step_finish') {
    // `tool-calls` reason just means "this turn used tools" — the actual
    // tool call/result lines already rendered. Suppress the noisy summary
    // and only print a final-turn marker when the run actually stopped.
    const part = obj.part as Record<string, unknown> | undefined
    const reason = (part?.reason as string) ?? ''
    if (reason === 'tool-calls' || reason === '' || reason === 'tool_use') {
      return null
    }
    const tokens = part?.tokens as
      | { input?: number; output?: number; total?: number }
      | undefined
    const bits: string[] = [reason]
    if (tokens?.input != null || tokens?.output != null) {
      bits.push(
        `${(tokens.input ?? 0).toLocaleString()} in / ${(tokens.output ?? 0).toLocaleString()} out`,
      )
    }
    return { kind: 'step_end', toolUseId: '', summary: bits.join(' · ') }
  }

  // OpenCode tool updates via message.part.updated → {part:{type:"tool",...}}
  if (type === 'message.part.updated') {
    const part = obj.part as Record<string, unknown> | undefined
    if (part?.type !== 'tool') return null
    return extractAiEvent(JSON.stringify({ type: 'tool', part }))
  }

  return null
}

// Dedup: Codex and OpenCode both emit repeat frames for the same tool (e.g.
// message.part.updated fires on every state tick). Suppress identical
// consecutive summaries within a short window.
let lastEventKey = ''
let lastEventAt = 0

function renderLogEntry(payload: LogPayload): void {
  const ts = colors.muted(formatTime(payload.created_at))

  if (payload.level !== 'ai_event') {
    console.log(`${ts} ${levelColor(payload.level)} ${payload.message}`)
    return
  }

  const event = extractAiEvent(payload.message)
  if (!event) {
    // Unknown ai_event type — drop rather than dump raw JSON.
    return
  }

  const key = `${event.kind}:${event.toolUseId}:${event.summary}`
  const now = Date.now()
  if (key === lastEventKey && now - lastEventAt < 2000) return
  lastEventKey = key
  lastEventAt = now

  switch (event.kind) {
    case 'thinking': {
      const text = event.summary.trim()
      if (!text) return
      console.log(`${ts} ${colors.muted('💭')} ${colors.muted(text)}`)
      return
    }
    case 'text': {
      const text = event.summary.trim()
      if (!text) return
      console.log(`${ts} ${colors.primary('▸')} ${text}`)
      return
    }
    case 'call': {
      const label = event.name ? colors.bold(event.name) : ''
      const arrow = colors.primary('→')
      console.log(
        `${ts} ${arrow} ${label}${label ? ' ' : ''}${event.summary}`,
      )
      return
    }
    case 'result': {
      const arrow = event.isError ? colors.error('←') : colors.muted('←')
      const body = event.isError
        ? colors.error(event.summary || '(error)')
        : colors.muted(event.summary || '(empty)')
      console.log(`${ts} ${arrow} ${body}`)
      return
    }
    case 'step': {
      // Silent — the turn boundary is implicit in subsequent events.
      return
    }
    case 'step_end': {
      if (!event.summary) return
      console.log(`${ts} ${colors.muted(`↳ ${event.summary}`)}`)
      return
    }
  }
}

function printRunSummary(run: AgentRunResponse): void {
  header(`${icons.info} Run #${run.id} summary`)
  keyValue('Status', statusColor(run.status))
  const workflowLabel =
    run.source === 'cli_ephemeral'
      ? `${run.agent_slug ?? 'ephemeral'} ${colors.muted('(ephemeral)')}`
      : (run.agent_slug ?? `(config #${run.config_id ?? '?'})`)
  keyValue('Workflow', workflowLabel)
  if (run.ai_provider) keyValue('Provider', run.ai_provider)
  if (run.ai_model) keyValue('Model', run.ai_model)
  if (run.tokens_input || run.tokens_output) {
    keyValue(
      'Tokens',
      `${run.tokens_input.toLocaleString()} in / ${run.tokens_output.toLocaleString()} out`,
    )
  }
  if (run.estimated_cost_cents > 0) {
    keyValue('Estimated cost', `$${(run.estimated_cost_cents / 100).toFixed(4)}`)
  }
  if (run.files_changed > 0) keyValue('Files changed', run.files_changed)
  if (run.pr_url) keyValue('Pull request', colors.primary(run.pr_url))
  if (run.preview_url) keyValue('Preview URL', colors.primary(run.preview_url))
  if (run.started_at && run.completed_at) {
    const ms = new Date(run.completed_at).getTime() - new Date(run.started_at).getTime()
    keyValue('Duration', `${(ms / 1000).toFixed(1)}s`)
  }
  if (run.error_message) {
    newline()
    errorOut(`Error: ${run.error_message}`)
  }
  if (run.analysis) {
    newline()
    header(`${icons.info} Analysis`)
    console.log(run.analysis)
  }
}
