import { randomUUID } from 'node:crypto'

type Fetch = typeof globalThis.fetch

interface ProviderOptions {
  id?: string
  config?: {
    aiProvider?: unknown
    model?: unknown
    thinkingLevel?: unknown
    permissionMode?: unknown
    timeoutMs?: unknown
  }
}

interface ConversationResponse {
  public_id: string
  ai_provider: string
}

interface ToolCall {
  name: string
  operation?: string
}

interface EvalMetadata {
  provider: string
  model?: string
  conversationPublicId: string
  toolCalls: ToolCall[]
  operations: string[]
  errors: string[]
  permissionRequests: string[]
}

interface PromptfooProviderResponse {
  output?: string
  error?: string
  cached?: boolean
  metadata?: EvalMetadata
}

interface SseEvent {
  event: string
  data: string
}

const ALLOWED_PROVIDERS = new Set(['claude_cli', 'codex_cli'])
const DEFAULT_TIMEOUT_MS = 5 * 60_000
const MAX_ERROR_BODY_LENGTH = 4_000

function requiredEnv(name: string): string {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function optionalString(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined
}

function parsePositiveInteger(value: unknown, fallback: number): number {
  const parsed = typeof value === 'number' ? value : Number(value)
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback
}

export function normalizeApiUrl(value: string): URL {
  const url = new URL(value)
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    throw new Error('TEMPS_URL must use http or https')
  }

  const isLoopback =
    url.hostname === 'localhost' ||
    url.hostname === '::1' ||
    /^127(?:\.\d{1,3}){3}$/.test(url.hostname)
  if (!isLoopback && process.env.TEMPS_AI_EVAL_ALLOW_REMOTE !== '1') {
    throw new Error(
      'Refusing to send TEMPS_API_KEY to a non-loopback host. Set TEMPS_AI_EVAL_ALLOW_REMOTE=1 for an intentional HTTPS target.',
    )
  }
  if (!isLoopback && url.protocol !== 'https:') {
    throw new Error('Remote AI eval targets must use HTTPS')
  }

  url.pathname = `${url.pathname.replace(/\/+$/, '').replace(/\/api$/, '')}/api`
  url.search = ''
  url.hash = ''
  return url
}

export function redactSensitiveText(value: string, apiKey = ''): string {
  let redacted = value
  if (apiKey) redacted = redacted.split(apiKey).join('[REDACTED]')
  return redacted
    .replace(/\bBearer\s+[^\s"']+/gi, 'Bearer [REDACTED]')
    .replace(/\b(?:tk_|sk-(?:ant-)?)\w[\w.-]{7,}/g, '[REDACTED]')
    .replace(
      /("(?:access_token|refresh_token|id_token|api_key|token|password|secret|credential)"\s*:\s*")[^"]+("\s*)/gi,
      '$1[REDACTED]$2',
    )
}

export function parseSse(text: string): SseEvent[] {
  const events: SseEvent[] = []
  for (const block of text.replace(/\r\n/g, '\n').split('\n\n')) {
    if (!block.trim()) continue
    let event = 'message'
    const data: string[] = []
    for (const line of block.split('\n')) {
      if (line.startsWith('event:')) event = line.slice(6).trim()
      if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''))
    }
    if (data.length > 0 || event === 'turn_complete') {
      events.push({ event, data: data.join('\n') })
    }
  }
  return events
}

function extractOperation(argumentsJson: string): string | undefined {
  try {
    const parsed = JSON.parse(argumentsJson) as { command?: unknown }
    if (typeof parsed.command !== 'string') return undefined
    const tokens = parsed.command.trim().split(/\s+/)
    return tokens[1] && !tokens[1].startsWith('-') ? tokens[1] : undefined
  } catch {
    return undefined
  }
}

function joinUrl(base: URL, path: string): URL {
  return new URL(`${base.toString().replace(/\/$/, '')}${path}`)
}

export default class TempsChatProvider {
  private readonly providerId: string
  private readonly aiProvider: string
  private readonly model?: string
  private readonly thinkingLevel?: string
  private readonly permissionMode?: string
  private readonly timeoutMs: number
  private readonly fetchImpl: Fetch

  constructor(options: ProviderOptions, fetchImpl: Fetch = globalThis.fetch) {
    this.providerId = options.id ?? 'temps-chat'
    this.aiProvider = optionalString(options.config?.aiProvider) ?? ''
    if (!ALLOWED_PROVIDERS.has(this.aiProvider)) {
      throw new Error('aiProvider must be claude_cli or codex_cli')
    }
    const providerEnvPrefix = this.aiProvider === 'claude_cli' ? 'CLAUDE' : 'CODEX'
    this.model =
      optionalString(options.config?.model) ??
      optionalString(process.env[`TEMPS_AI_EVAL_${providerEnvPrefix}_MODEL`])
    this.thinkingLevel =
      optionalString(options.config?.thinkingLevel) ??
      optionalString(process.env[`TEMPS_AI_EVAL_${providerEnvPrefix}_THINKING`])
    this.permissionMode = optionalString(options.config?.permissionMode)
    this.timeoutMs = parsePositiveInteger(options.config?.timeoutMs, DEFAULT_TIMEOUT_MS)
    this.fetchImpl = fetchImpl
  }

  id(): string {
    return this.providerId
  }

  async callApi(prompt: string): Promise<PromptfooProviderResponse> {
    const apiKey = requiredEnv('TEMPS_API_KEY')
    const projectId = requiredEnv('TEMPS_AI_EVAL_PROJECT_ID')
    if (!/^\d+$/.test(projectId)) throw new Error('TEMPS_AI_EVAL_PROJECT_ID must be an integer')
    const apiUrl = normalizeApiUrl(requiredEnv('TEMPS_URL'))
    const contextId = `promptfoo-${this.aiProvider}-${randomUUID()}`
    const signal = AbortSignal.timeout(this.timeoutMs)
    let conversation: ConversationResponse | undefined

    try {
      conversation = await this.requestJson<ConversationResponse>(
        joinUrl(apiUrl, `/projects/${projectId}/ai/conversations`),
        apiKey,
        signal,
        {
          method: 'POST',
          body: JSON.stringify({
            context_type: 'project',
            context_id: contextId,
            ai_provider: this.aiProvider,
            ...(this.model ? { ai_model: this.model } : {}),
            ...(this.thinkingLevel ? { ai_thinking_level: this.thinkingLevel } : {}),
            ...(this.permissionMode ? { ai_permission_mode: this.permissionMode } : {}),
          }),
        },
      )
      if (conversation.ai_provider !== this.aiProvider) {
        throw new Error(
          `Temps created the eval conversation with ${conversation.ai_provider}, expected ${this.aiProvider}`,
        )
      }

      const response = await this.fetchImpl(
        joinUrl(
          apiUrl,
          `/projects/${projectId}/ai/conversations/${encodeURIComponent(conversation.public_id)}/messages`,
        ),
        {
          method: 'POST',
          headers: this.headers(apiKey, 'text/event-stream'),
          body: JSON.stringify({
            content: prompt,
            ...(this.model ? { ai_model: this.model } : {}),
            ...(this.thinkingLevel ? { ai_thinking_level: this.thinkingLevel } : {}),
            ...(this.permissionMode ? { ai_permission_mode: this.permissionMode } : {}),
          }),
          signal,
        },
      )
      if (!response.ok) await this.throwHttpError(response, apiKey)

      const sseEvents = parseSse(await response.text())
      const tokenText: string[] = []
      const toolCalls: ToolCall[] = []
      const errors: string[] = []
      const permissionRequests: string[] = []
      for (const event of sseEvents) {
        if (event.event === 'message') tokenText.push(event.data)
        if (event.event === 'error') errors.push(redactSensitiveText(event.data, apiKey))
        if (event.event === 'permission_requested') {
          // Never persist the raw `input`: provider-native approval payloads
          // can contain command arguments or other user data. The eval only
          // needs enough metadata to fail this unexpected state.
          try {
            const permission = JSON.parse(event.data) as { kind?: unknown; tool_name?: unknown }
            const kind = typeof permission.kind === 'string' ? permission.kind : 'unknown'
            const toolName =
              typeof permission.tool_name === 'string' ? permission.tool_name : 'unknown'
            permissionRequests.push(`${kind}:${toolName}`)
          } catch {
            permissionRequests.push('unknown')
          }
        }
        if (event.event === 'tool_call') {
          try {
            const tool = JSON.parse(event.data) as { name?: unknown; arguments?: unknown }
            if (typeof tool.name === 'string') {
              const argumentsJson = typeof tool.arguments === 'string' ? tool.arguments : '{}'
              toolCalls.push({ name: tool.name, operation: extractOperation(argumentsJson) })
            }
          } catch {
            errors.push('Temps returned an invalid tool_call SSE event')
          }
        }
      }

      const history = await this.requestJson<{
        messages?: Array<{ role?: string; content?: string }>
      }>(
        joinUrl(
          apiUrl,
          `/projects/${projectId}/ai/conversations/${encodeURIComponent(conversation.public_id)}`,
        ),
        apiKey,
        signal,
      )
      const persistedOutput = history.messages
        ?.filter((message) => message.role === 'assistant' && typeof message.content === 'string')
        .at(-1)?.content
      const output = redactSensitiveText(persistedOutput || tokenText.join(''), apiKey).trim()
      const metadata: EvalMetadata = {
        provider: this.aiProvider,
        ...(this.model ? { model: this.model } : {}),
        conversationPublicId: conversation.public_id,
        toolCalls,
        operations: [...new Set(toolCalls.flatMap((call) => (call.operation ? [call.operation] : [])))],
        errors,
        permissionRequests,
      }
      if (errors.length > 0) {
        return { error: errors.join('\n'), output, cached: false, metadata }
      }
      if (permissionRequests.length > 0) {
        return {
          error: 'The read-only eval unexpectedly paused for interactive permission.',
          output,
          cached: false,
          metadata,
        }
      }
      if (!output) {
        return { error: 'Temps completed the turn without an assistant response.', cached: false, metadata }
      }
      return { output, cached: false, metadata }
    } catch (error) {
      return {
        error: redactSensitiveText(error instanceof Error ? error.message : String(error), apiKey),
        cached: false,
      }
    } finally {
      if (conversation && process.env.TEMPS_AI_EVAL_KEEP_CONVERSATIONS !== '1') {
        await this.archive(apiUrl, projectId, conversation.public_id, apiKey).catch((error) => {
          // Do not include the URL, conversation id, body, or authorization
          // header in cleanup diagnostics. The status-only error from archive
          // is enough to tell the operator that a transcript may remain.
          console.warn(
            `[Temps AI eval cleanup] ${
              error instanceof Error ? error.message : 'failed to archive temporary conversation'
            }`,
          )
        })
      }
    }
  }

  private headers(apiKey: string, accept = 'application/json'): HeadersInit {
    return {
      Accept: accept,
      Authorization: `Bearer ${apiKey}`,
      'Content-Type': 'application/json',
    }
  }

  private async requestJson<T>(
    url: URL,
    apiKey: string,
    signal: AbortSignal,
    init: RequestInit = {},
  ): Promise<T> {
    const response = await this.fetchImpl(url, {
      ...init,
      headers: this.headers(apiKey),
      signal,
    })
    if (!response.ok) await this.throwHttpError(response, apiKey)
    return (await response.json()) as T
  }

  private async throwHttpError(response: Response, apiKey: string): Promise<never> {
    const body = (await response.text()).slice(0, MAX_ERROR_BODY_LENGTH)
    throw new Error(
      `Temps API request failed (HTTP ${response.status}): ${redactSensitiveText(body, apiKey)}`,
    )
  }

  private async archive(
    apiUrl: URL,
    projectId: string,
    publicId: string,
    apiKey: string,
  ): Promise<void> {
    const response = await this.fetchImpl(
      joinUrl(
        apiUrl,
        `/projects/${projectId}/ai/conversations/${encodeURIComponent(publicId)}/archive`,
      ),
      {
        method: 'POST',
        headers: this.headers(apiKey),
        signal: AbortSignal.timeout(15_000),
      },
    )
    if (!response.ok) {
      throw new Error(`failed to archive temporary conversation (HTTP ${response.status})`)
    }
  }
}
