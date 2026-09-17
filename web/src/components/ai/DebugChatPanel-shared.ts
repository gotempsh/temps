// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ConversationDetailResponse } from '@/api/client'
import { type PermissionRequest } from '@/components/ai/PermissionCard'

import {
  shouldAutoRefreshHarnessModels,
  type ChatProviderOption,
} from './chat-runtime-options'

import {
  upsertMessageTool,
  type ChatMessage,
  type ChatAttachment,
  type ChatPart,
  type ToolCall,
} from './chat-message-parts'

/** A minimal mdast node (only the fields this file touches). */
export interface MdNode {
  type: string
  value?: string
  children?: MdNode[]
}

export function shouldPreserveRuntimeSelectionAfterProviderLoad(
  providerPinned: boolean,
  explicitRefresh: boolean
): boolean {
  return providerPinned || explicitRefresh
}

export type ProviderRefreshState = {
  providerId: string
  operation: 'workspace_models' | 'provider_status'
}

export function providerRefreshMatchesSelection(
  refresh: ProviderRefreshState | null,
  providerId: string | undefined
): boolean {
  return refresh !== null && refresh.providerId === providerId
}

export function providerRefreshCopy(
  refresh: ProviderRefreshState | null,
  providerId: string | undefined,
  providerName: string
): { status: string; button: string } | null {
  if (!refresh || refresh.providerId !== providerId) return null

  return refresh.operation === 'workspace_models'
    ? {
        status: `Starting your workspace and resolving models for ${providerName}…`,
        button: 'Starting…',
      }
    : {
        status: `Refreshing authentication and models for ${providerName}…`,
        button: 'Refreshing…',
      }
}

export function claimAutomaticModelRefresh(
  attempts: Set<string>,
  contextType: string,
  provider: ChatProviderOption | undefined
): boolean {
  if (!provider) return false
  const attemptKey = `${contextType}:${provider.id}`
  if (
    !shouldAutoRefreshHarnessModels(
      contextType,
      provider,
      attempts.has(attemptKey)
    )
  ) {
    return false
  }
  attempts.add(attemptKey)
  return true
}

/**
 * Maps a `getConversation` response into the panel's `ChatMessage[]` state,
 * including a still-unresolved permission request (ADR-038 Phase 2) as a live,
 * answerable card rather than the inert "asked" text alone — used both on
 * initial load and by `pollForReply`'s fallback refetch.
 */
export function mapConversationDetail(detail: {
  messages?: Array<{
    cursor?: string
    role: string
    content: string
    created_at?: string
    tools?: ChatMessage['tools'] | null
    parts?: ChatPart[] | null
    attachments?: ChatAttachment[] | null
  }> | null
  pending_permission?: PermissionRequest | null
  turn_status?: string | null
}): ChatMessage[] {
  const interrupted =
    !detail.pending_permission &&
    ['completed', 'failed', 'cancelled', 'canceled', 'interrupted'].includes(
      detail.turn_status ?? ''
    )
  const finishTool = (tool: ToolCall): ToolCall =>
    interrupted && tool.result == null
      ? {
          ...tool,
          result: JSON.stringify({
            is_error: true,
            status: 'interrupted',
            error:
              'Tool interrupted: the turn ended before a result was received.',
          }),
        }
      : tool
  const mapped: ChatMessage[] = (detail.messages ?? []).map((m) => {
    const rawParts = m.parts?.map((part): ChatPart =>
      part.type === 'tool' ? { ...part, tool: finishTool(part.tool) } : part
    )
    return {
      server_cursor: m.cursor,
      role: m.role,
      content: m.content,
      created_at: m.created_at,
      tools: m.tools?.map(finishTool) ?? undefined,
      parts: rawParts && rawParts.length > 0 ? rawParts : undefined,
      attachments: m.attachments ?? undefined,
    }
  })
  const pendingPermission = detail.pending_permission
  if (pendingPermission) {
    const last = mapped[mapped.length - 1]
    const permissionPart: ChatPart = {
      type: 'permission',
      permission: pendingPermission,
    }
    if (last?.role === 'assistant') {
      last.parts = [...(last.parts ?? []), permissionPart]
    } else {
      mapped.push({
        role: 'assistant',
        content: '',
        created_at: new Date().toISOString(),
        parts: [permissionPart],
      })
    }
  }
  return mapped
}

export type ConversationHistoryPage = {
  has_more: boolean
  next_before?: string | null
}

export type PaginatedConversationDetail = ConversationDetailResponse & {
  page?: ConversationHistoryPage
}

/**
 * Pop the trailing optimistic assistant turn if it never received anything.
 * Checking `content` alone isn't enough: a pending permission card lives in
 * `parts`, not `content` — dropping the turn on a dead connection would
 * silently discard a still-answerable question with no way to resolve it
 * (ADR-038 Phase 2).
 */
export type SetMessages = React.Dispatch<React.SetStateAction<ChatMessage[]>>

export interface ChatFailure {
  code: string
  title: string
  detail: string
  retryable: boolean
}

export type SetChatFailure = React.Dispatch<
  React.SetStateAction<ChatFailure | null>
>

export const UNKNOWN_CHAT_FAILURE: ChatFailure = {
  code: 'harness_failed',
  title: 'AI harness failed',
  detail:
    'The selected harness stopped before Temps received a reply. Retry once; if it repeats, check its authentication, selected model, and the server logs.',
  retryable: true,
}

export function localChatFailure(
  title: string,
  detail: string,
  code = 'chat_request_failed',
  retryable = true
): ChatFailure {
  return { code, title, detail, retryable }
}

export function isSafeFailureText(
  value: unknown,
  maxLength: number
): value is string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > maxLength
  )
    return false
  return !/(?:\/run\/secrets|\/home\/|\/Users\/|\/tmp\/|authorization\s*:|bearer\s+|api[_ -]?key\s*[:=]|token\s*[:=]|tmcp_|sk-[A-Za-z0-9])/i.test(
    value
  )
}

/** Decode the browser-safe error envelope sent on the conversation wire.
 * Unknown/legacy payloads are intentionally not echoed: older servers could
 * include subprocess paths or credential-shaped provider diagnostics. */
export function parseChatFailure(data: string): ChatFailure {
  try {
    const parsed = JSON.parse(data) as Partial<ChatFailure>
    if (
      isSafeFailureText(parsed.code, 80) &&
      isSafeFailureText(parsed.title, 120) &&
      isSafeFailureText(parsed.detail, 600) &&
      typeof parsed.retryable === 'boolean'
    ) {
      return {
        code: parsed.code,
        title: parsed.title,
        detail: parsed.detail,
        retryable: parsed.retryable,
      }
    }
  } catch {
    // A legacy server may send a raw provider error. Never render it.
  }
  return UNKNOWN_CHAT_FAILURE
}

/** Restore the durable failure, including when its live event was missed. */
export function conversationFailure(detail: {
  turn_status?: string
  failure?: ChatFailure | null
}): ChatFailure | null {
  if (detail.turn_status !== 'failed') return null
  if (detail.failure) return parseChatFailure(JSON.stringify(detail.failure))
  return {
    code: 'harness_failure_details_unavailable',
    title: 'This turn failed',
    detail:
      'The error explanation was not retained for this turn. Retry the message to get a current result or an error explanation.',
    retryable: true,
  }
}

export function chatFailureFromProblem(
  problem: unknown,
  status?: number
): ChatFailure {
  if (status === 409) {
    return localChatFailure(
      'A turn is already running',
      'This conversation is already processing a message. Wait for it to finish or stop it before retrying.',
      'turn_already_running'
    )
  }
  const value = problem as { title?: unknown; detail?: unknown } | null
  if (
    value &&
    isSafeFailureText(value.title, 120) &&
    isSafeFailureText(value.detail, 600)
  ) {
    return {
      code: status ? `http_${status}` : 'chat_request_failed',
      title: value.title,
      detail: value.detail,
      retryable: status == null || status >= 500,
    }
  }
  return UNKNOWN_CHAT_FAILURE
}

export function dropEmptyAssistantTurn(setMessages: SetMessages) {
  setMessages((m) => {
    const last = m[m.length - 1]
    return last?.role === 'assistant' &&
      last.content === '' &&
      !(last.parts && last.parts.length > 0)
      ? m.slice(0, -1)
      : m
  })
}

export function permissionModeIsAuto(permissionModeId: string | null) {
  return permissionModeId === 'auto' || permissionModeId === 'full-access'
}

export function permissionModeOptionDisabled(
  turnActive: boolean,
  permissionModeId: string
) {
  return turnActive && !permissionModeIsAuto(permissionModeId)
}

export function turnStateNeedsResync(status?: string) {
  return status === 'running'
}

export function clearResolvedPermissionParts(
  messages: ChatMessage[],
  resolvedPermissionIds: string[]
) {
  const resolved = new Set(resolvedPermissionIds)
  return messages.map((message) => ({
    ...message,
    parts: message.parts?.filter(
      (part) => part.type !== 'permission' || !resolved.has(part.permission.id)
    ),
  }))
}

export function appendLiveUserTurn(
  messages: ChatMessage[],
  user: {
    content: string
    created_at?: string
    turn_id?: string
    attachments?: ChatAttachment[]
  }
) {
  if (
    user.turn_id &&
    messages.some((message) => message.client_turn_id === user.turn_id)
  ) {
    return messages
  }
  const createdAt = user.created_at ?? new Date().toISOString()
  return [
    ...messages,
    {
      role: 'user',
      content: user.content,
      attachments: user.attachments,
      created_at: createdAt,
      client_turn_id: user.turn_id,
    },
    {
      role: 'assistant',
      content: '',
      created_at: createdAt,
      client_turn_id: user.turn_id,
    },
  ]
}

/**
 * Apply one WebSocket event to the trailing assistant turn. Message submission
 * is a short-lived HTTP command; all live token/tool/permission output has one
 * transport and therefore cannot be duplicated by an SSE echo.
 */
export function applyWireEvent(
  eventName: string,
  data: string,
  setMessages: SetMessages,
  setError: SetChatFailure
) {
  if (eventName === 'error') {
    setError(parseChatFailure(data))
    dropEmptyAssistantTurn(setMessages)
    return
  }
  if (eventName === 'tool_call') {
    try {
      const t = JSON.parse(data) as {
        id: string
        name: string
        arguments: string
      }
      if (
        typeof t.id !== 'string' ||
        typeof t.name !== 'string' ||
        typeof t.arguments !== 'string'
      )
        return
      setMessages((m) => {
        const copy = [...ensureRunningAssistant(m, true)]
        const last = copy[copy.length - 1]
        if (last?.role === 'assistant') {
          const tool: ToolCall = {
            id: t.id,
            name: t.name,
            arguments: t.arguments,
            result: undefined,
          }
          copy[copy.length - 1] = upsertMessageTool(last, tool)
        }
        return copy
      })
    } catch {
      /* ignore malformed tool_call frame */
    }
    return
  }
  if (eventName === 'tool_result') {
    try {
      const t = JSON.parse(data) as {
        id: string
        name: string
        content: string
      }
      if (
        typeof t.id !== 'string' ||
        typeof t.name !== 'string' ||
        typeof t.content !== 'string'
      )
        return
      setMessages((m) => {
        const copy = [...ensureRunningAssistant(m, true)]
        const last = copy[copy.length - 1]
        if (last?.role === 'assistant') {
          copy[copy.length - 1] = upsertMessageTool(last, {
            id: t.id,
            name: t.name,
            arguments: '',
            result: t.content,
          })
        }
        return copy
      })
    } catch {
      /* ignore malformed tool_result frame */
    }
    return
  }
  // ADR-038 Phase 2: interactive bridge permission request
  if (eventName === 'permission_requested') {
    try {
      const p = JSON.parse(data) as {
        id: string
        kind: string
        tool_name: string
        input: unknown
      }
      const perm: PermissionRequest = {
        id: p.id,
        kind: p.kind as PermissionRequest['kind'],
        tool_name: p.tool_name,
        input: p.input,
      }
      setMessages((m) => {
        const copy = [...m]
        const last = copy[copy.length - 1]
        if (last?.role === 'assistant') {
          copy[copy.length - 1] = {
            ...last,
            parts: [
              ...(last.parts ?? []),
              { type: 'permission', permission: perm },
            ],
          }
        }
        return copy
      })
    } catch {
      /* ignore malformed permission_requested frame */
    }
    return
  }
  // Plain token text.
  if (data) {
    setMessages((m) => {
      const copy = [...m]
      const last = copy[copy.length - 1]
      // Append to the trailing text part, or open a new one (so prose that
      // follows a tool call becomes its own segment).
      const prevParts = last?.parts ?? []
      const lastPart = prevParts[prevParts.length - 1]
      const parts: ChatPart[] =
        lastPart?.type === 'text'
          ? [
              ...prevParts.slice(0, -1),
              { type: 'text', text: lastPart.text + data },
            ]
          : [...prevParts, { type: 'text', text: data }]
      copy[copy.length - 1] = {
        ...last,
        role: 'assistant',
        content: (last?.content ?? '') + data,
        parts,
        created_at: last?.created_at ?? new Date().toISOString(),
      }
      return copy
    })
  }
}

/**
 * Human label for a tool card — what the tool actually did. For the `temps` and
 * `temps_write` virtual CLIs that's the command it ran (e.g.
 * `traces get_trace --trace_id …`, or `trigger_project_pipeline --environment_id 8`),
 * which is far more useful than several identical "temps"/"temps_write" rows.
 * Falls back to the tool name for other tools or unparsable args.
 */
export function toolLabel(tool: ToolCall): string {
  const summary = toolOperationLabel(tool)
  if (!tool.name.startsWith('mcp__')) return summary
  const [, server, ...functionParts] = tool.name.split('__')
  const functionName = functionParts.join('__')
  if (!server || !functionName) return summary
  const identity = `${server} · ${functionName}`
  if (summary === tool.name || summary === functionName) return identity
  const details = summary.startsWith(`${functionName} · `)
    ? summary.slice(functionName.length + 3)
    : summary
  return `${identity} · ${details}`
}

export function toolOperationLabel(tool: ToolCall): string {
  // MCP clients qualify tool names as `mcp__<server>__<tool>`. The chat wire
  // persists that provider-native name so it can be inspected later, but the
  // compact row also describes the operation; toolLabel retains MCP identity.
  const qualifiedNameParts = tool.name.split('__')
  const baseName =
    qualifiedNameParts[qualifiedNameParts.length - 1] || tool.name

  const processLabels: Record<string, string> = {
    temps_process_start: 'Start process',
    temps_process_status: 'Process status',
    temps_process_logs: 'Process logs',
    temps_process_stop: 'Stop process',
    temps_process_restart: 'Restart process',
  }
  const processLabel = Object.prototype.hasOwnProperty.call(
    processLabels,
    baseName
  )
    ? processLabels[baseName]
    : undefined
  if (processLabel) {
    try {
      const input: unknown = JSON.parse(tool.arguments)
      if (typeof input === 'object' && input !== null) {
        const args = input as Record<string, unknown>
        const target =
          baseName === 'temps_process_start' ? args.name : args.process_id
        const details = [
          target,
          baseName === 'temps_process_start' ? args.program : undefined,
        ]
          .filter(
            (value): value is string =>
              typeof value === 'string' && value.trim().length > 0
          )
          .map((value) => value.trim())
        if (details.length) return `${processLabel} · ${details.join(' · ')}`
      }
    } catch {
      // Keep the operation visible while streamed arguments are incomplete.
    }
    return processLabel
  }

  if (baseName === 'temps' || baseName === 'temps_write') {
    try {
      const args = JSON.parse(tool.arguments) as {
        command?: unknown
        commands?: unknown
      }
      if (typeof args.command === 'string' && args.command.trim()) {
        return args.command.trim()
      }
      if (Array.isArray(args.commands)) {
        const commands = args.commands.filter(
          (command): command is string =>
            typeof command === 'string' && Boolean(command.trim())
        )
        if (commands.length > 0) {
          return `${commands.length} commands · ${commands
            .map((command) => command.trim())
            .join(' → ')}`
        }
      }
    } catch {
      /* fall through to the tool name */
    }
  }
  // Native harness events (Claude Code today) use the same tool card. Surface
  // the command directly so a sequence of `Bash` calls is useful at a glance;
  // the full, redacted arguments remain available when expanded.
  if (baseName.toLowerCase() === 'bash') {
    try {
      const args = JSON.parse(tool.arguments) as { command?: unknown }
      if (typeof args.command === 'string' && args.command.trim()) {
        return `Bash · ${args.command.trim().split('\n')[0]}`
      }
    } catch {
      /* fall through to the tool name */
    }
  }
  // Native filesystem tools otherwise become a wall of indistinguishable
  // cards. The path is safe to surface here: it is already part of the
  // redacted native tool event, and it gives the person reviewing the turn a
  // precise answer to "what did it touch?" without opening every card.
  if (['read', 'edit', 'write'].includes(baseName.toLowerCase())) {
    try {
      const args = JSON.parse(tool.arguments) as Record<string, unknown>
      const path =
        args.file_path ?? args.filePath ?? args.path ?? args.target_file
      if (typeof path === 'string' && path.trim()) {
        return `${baseName} · ${path.trim()}`
      }
    } catch {
      /* fall through to the tool name */
    }
  }
  try {
    const input = JSON.parse(tool.arguments)
    const summary =
      typeof input === 'object' && input !== null
        ? (input.command ??
          input.query ??
          input.description ??
          input.path ??
          JSON.stringify(input))
        : input
    if (typeof summary === 'string' && summary && summary !== '{}') {
      return `${baseName} · ${summary}`
    }
  } catch {
    // Partial streamed JSON is available under Input once complete.
  }
  return tool.name
}

/** A process receipt is a lifecycle observation, never an HTTP readiness claim. */
export function processToolSummary(tool: ToolCall): string | undefined {
  const name = tool.name.split('__').pop()
  if (
    !name ||
    ![
      'temps_process_start',
      'temps_process_status',
      'temps_process_logs',
      'temps_process_stop',
      'temps_process_restart',
    ].includes(name) ||
    !tool.result
  )
    return undefined
  try {
    const receipt = JSON.parse(tool.result)
    if (!receipt || typeof receipt !== 'object') return undefined
    if (
      receipt.type === 'process' &&
      receipt.process &&
      typeof receipt.process === 'object'
    ) {
      const process = receipt.process
      const states: Record<string, string> = {
        starting: 'Starting',
        running: 'Running',
        stopped: 'Stopped',
        stopping: 'Stopping',
        exited: 'Exited',
        failed: 'Failed',
        restarting: 'Restarting',
        queued: 'Queued',
        succeeded: 'Exited successfully',
        cancelled: 'Stopped',
      }
      if (
        typeof process.status !== 'string' ||
        !Object.prototype.hasOwnProperty.call(states, process.status)
      )
        return undefined
      const pid =
        Number.isInteger(process.pid) && process.pid > 0
          ? ` · PID ${process.pid}`
          : ''
      const failure =
        process.status === 'failed' &&
        typeof process.detail === 'string' &&
        process.detail.trim()
          ? ` · ${process.detail.trim().slice(0, 240)}`
          : ''
      return `${states[process.status]}${pid}${failure}`
    }
    if (receipt.type === 'logs' && Array.isArray(receipt.lines)) {
      return `${receipt.lines.length} log ${receipt.lines.length === 1 ? 'line' : 'lines'}${receipt.truncated === true ? ' · Limited output' : ''}`
    }
  } catch {
    // Unknown/older result formats remain available in the expanded result.
  }
  return undefined
}

/** The proposal payload a `temps_write` tool result carries (JSON string). */
export interface Proposal {
  action_id: string
  operation: string
  method: string
  summary: string
}

/** One step of a multi-step plan proposal. */
export interface PlanStep {
  action_id: string
  operation: string
  method: string
  summary: string
  step: number
}

export interface PlanProposal {
  plan_id: string | null
  steps: PlanStep[]
}

export interface StepState {
  status: string
  result?: string | null
  error?: string | null
  params?: string | null
}

/**
 * ADR-038 Phase 1/2: a persistent notice about the interactive-tools state.
 *
 * - `null` / missing / unknown: render nothing (unconfirmed guess is worse)
 * - `supports_interactive_tools === false` AND `interactive_bridge_status`
 *   is `null` / absent: original Phase 1 notice — conversational mode, no
 *   bridge opted in.
 * - `interactive_bridge_status === 'unavailable'`: amber notice — bridge
 *   opted in but CLI not authenticated right now (falling back).
 * - `interactive_bridge_status === 'healthy'`: bridge is live, hide the
 *   notice entirely — `PermissionCard`s will render in-line as they arrive.
 */
export function applicationHarnessPermissionNotice(
  provider: string,
  permissionMode: string | null
): string | null {
  if (permissionMode === 'auto' || permissionMode === 'full-access') return null
  // Claude's print-mode permission prompt is bridged through the turn-scoped
  // Temps MCP server. Native Write/Edit/Bash prompts therefore arrive as
  // PermissionCards and resume the same harness turn after the user decides.
  if (provider === 'claude_cli') return null
  return 'This harness does not yet expose its native approval prompts inline. Choose Auto to run commands inside the Temps sandbox without per-command approval.'
}

export interface DebugChatPanelProps {
  /** Present for legacy project-attached chat. AI workspace threads are user-owned. */
  projectId?: number
  /** Existing user-owned thread selected by the AI workspace. */
  conversationPublicId?: string
  /** Use user-rooted routes whose authority comes from the current principal. */
  userScoped?: boolean
  /** The interaction this chat is attached to, e.g. 'deployment' | 'alert'. */
  contextType: string
  contextId: string | number
  /** Auto-asked when a new chat is started, so it opens already working. */
  startPrompt?: string
  /** Create + seed the conversation automatically if none exists yet. */
  autoStart?: boolean
  /** Placeholder for the follow-up input. */
  placeholder?: string
  /**
   * Create the conversation lazily on the first user message instead of
   * requiring an explicit "Start" action. Used for free-form chats (e.g. a new
   * project chat) where there's nothing to auto-diagnose: the composer is live
   * immediately and the first send seeds the conversation.
   */
  lazyCreate?: boolean
  /** Friendly empty-state line shown for a lazy-create chat before any message. */
  emptyHint?: string
  /** Notifies the parent of the active conversation's public id (for reset). */
  onConversationChange?: (publicId: string | null) => void
  /** Typed live-wire invalidation events consumed by adjacent generated UI. */
  onLiveEvent?: (eventName: string, data: string) => void
  /** Re-read the conversation summary after a message/stop command settles. */
  onConversationStatusInvalidated?: () => void
  /** Show persisted history without exposing mutation controls. */
  readOnly?: boolean
  /** Prevent new turns without hiding saved history or the composer draft. */
  runtimeUpdateRequired?: boolean
}

export function chatApiPaths(userScoped: boolean, projectId?: number) {
  if (userScoped) {
    return {
      conversations: '/api/ai/conversations',
      pendingActions: '/api/ai/pending-actions',
    }
  }
  if (projectId == null) {
    throw new Error('A project-scoped chat requires a project id.')
  }
  return {
    conversations: `/api/projects/${projectId}/ai/conversations`,
    pendingActions: `/api/projects/${projectId}/ai/pending-actions`,
  }
}

export function conversationHistoryErrorMessage(status?: number): string {
  const suffix = status ? ` (HTTP ${status})` : ''
  return `Couldn’t load this conversation${suffix}. Its messages remain stored in Temps; reconnect and try again.`
}

/** The distance from the bottom that still counts as following the transcript. */
export const CHAT_SCROLL_BOTTOM_THRESHOLD_PX = 72

/**
 * Keep live output pinned only while the reader is already at the bottom.
 * A user who scrolls up owns the viewport until they explicitly return.
 */
export function isChatTranscriptNearBottom(
  viewport: Pick<HTMLElement, 'scrollHeight' | 'scrollTop' | 'clientHeight'>,
  threshold = CHAT_SCROLL_BOTTOM_THRESHOLD_PX
): boolean {
  return (
    viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight <=
    threshold
  )
}

/** Calculate display time from the server-owned turn timestamp, never mount time. */
export function serverElapsedDeciseconds(
  startedAt: string | null | undefined,
  nowMs: number
): number {
  if (!startedAt) return 0
  const startedAtMs = Date.parse(startedAt)
  if (!Number.isFinite(startedAtMs)) return 0
  return Math.max(0, Math.floor((nowMs - startedAtMs) / 100))
}

/**
 * Describe observable work rather than guessing at the model's internal state.
 * Keep the infrastructure boundary out of the primary status copy. Users care
 * that Temps is working in their durable project/workspace, not which runtime
 * container currently hosts that work.
 */
export function chatTurnActivityLabel(
  contextType: string,
  preparing: boolean
): string {
  if (contextType === 'application') {
    return preparing ? 'Preparing workspace' : 'Working on your project'
  }
  if (contextType === 'global') {
    return preparing ? 'Preparing workspace' : 'Working in your workspace'
  }
  return 'Working'
}

/** Streaming content does not imply completion; only the server terminal state does. */
export function shouldShowAssistantActivityAfterContent(
  partCount: number,
  streaming: boolean
) {
  return streaming && partCount > 0
}

export const DISCONNECTED_CHAT_POLL_INTERVAL_MS = 2000

export type ConversationTransportState =
  'connecting' | 'connected' | 'unavailable'

/**
 * WebSocket events are authoritative while the transport is connected. Full
 * transcript polling is reserved for a running turn after reconnect attempts
 * are exhausted; terminal turns and healthy sockets never poll.
 */
export function conversationSnapshotPollInterval(
  transport: ConversationTransportState,
  turnRunning: boolean
): number | false {
  return transport === 'unavailable' && turnRunning
    ? DISCONNECTED_CHAT_POLL_INTERVAL_MS
    : false
}

/** A disconnected observer wire must never be represented as model activity. */
export function shouldShowLiveTurn(
  streaming: boolean,
  wsTurnActive: boolean,
  liveUpdatesUnavailable: boolean
) {
  // `streaming` is the short message-submission command. Once accepted, the
  // persisted/live-wire state takes over until the terminal event arrives.
  return streaming || (!liveUpdatesUnavailable && wsTurnActive)
}

/** Restore activity from the persisted server snapshot after a remount. */
export function hasRunningServerTurn(value: { turn_status?: string } | null) {
  return value?.turn_status === 'running'
}

/**
 * A refreshed observer has persisted history but no optimistic empty assistant
 * message. Give the authoritative running turn its own trailing activity row
 * until the first assistant content arrives over the live stream.
 */
export function needsTrailingActivityRow(
  liveTurn: boolean,
  trailingRole?: ChatMessage['role']
) {
  return liveTurn && trailingRole !== 'assistant'
}

/** Permission polling suppresses only its synthetic user echo, never lifecycle events. */
export function shouldSuppressPermissionPollEvent(
  eventName: string,
  suppressionCount: number
) {
  return suppressionCount > 0 && eventName === 'user_message'
}

/** A server-owned terminal snapshot wins even when optimistic message counts match. */
export function permissionPollIsTerminal(
  turnStatus: string | undefined,
  hasPendingPermission: boolean
) {
  return turnStatus !== 'running' && !hasPendingPermission
}

/** Give resumed live events a concrete assistant target for tokens and tools. */
export function ensureRunningAssistant(
  messages: ChatMessage[],
  running: boolean
) {
  if (!running || messages[messages.length - 1]?.role === 'assistant') {
    return messages
  }
  return [
    ...messages,
    {
      role: 'assistant',
      content: '',
      created_at: new Date().toISOString(),
    },
  ]
}
