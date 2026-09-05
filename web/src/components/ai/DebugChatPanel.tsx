// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createConversation,
  findConversation,
  getAiProviderStatus,
  getConversation,
  listAiProviders,
  refreshAiProviderStatus,
  type ConversationDetailResponse,
  type ConversationResponse,
} from '@/api/client'
import {
  PermissionCard,
  type PermissionRequest,
} from '@/components/ai/PermissionCard'
import {
  createdServiceId,
  GeneratedServiceProposal,
  serviceProposalViewModel,
} from '@/components/ai/GeneratedServiceProposal'
import { GeneratedDeploymentCard } from '@/components/ai/GeneratedDeploymentCard'
import { GeneratedProjectCollection } from '@/components/ai/GeneratedProjectCollection'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  Check,
  ArrowDown,
  Brain,
  ChevronDown,
  ChevronRight,
  FileText,
  Image as ImageIcon,
  Info,
  Lock,
  Loader2,
  Paperclip,
  RefreshCw,
  Send,
  ShieldCheck,
  Shield,
  Sparkles,
  Square,
  Wrench,
  X,
} from 'lucide-react'
import {
  untrustedMarkdownImage,
  untrustedMarkdownLink,
} from '@/components/markdown/untrusted'
import { CopyButton } from '@/components/ui/copy-button'
import {
  SearchableSelect,
  type SearchableSelectOption,
} from '@/components/ui/searchable-select'
import { cn } from '@/lib/utils'
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import { toast } from 'sonner'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import rehypeHighlight from 'rehype-highlight'
import { useAiAssistant } from './AiAssistantContext'
import {
  chatHarnessProviderOptions,
  chatModelLabel,
  chatPermissionLabel,
  chatProviderLabel,
  chatThinkingItemContent,
  reconcileChatRuntimeAfterRefresh,
  resolveChatRuntimeSelection,
  usesHarnessCatalog,
  type ChatProviderOption,
  type ChatRuntimeSelection,
} from './chat-runtime-options'
import {
  chatComposerSubmitAction,
  isChatComposerDisabled,
  resolveChatComposerLayout,
} from './chat-page-state'
import {
  assistantParts,
  isTempsWriteToolName,
  type ChatMessage,
  type ChatAttachment,
  type ChatPart,
  type ToolCall,
} from './chat-message-parts'
import {
  prependHistoryPage,
  reconcileLatestHistoryPage,
  restoredHistoryScrollTop,
  shouldLoadEarlierMessages,
} from './chat-history-pagination'
import {
  projectCollectionFromApplicationProjectWrite,
  projectCollectionFromTool,
} from './tool-result-presentation'
// highlight.js token theme for fenced code blocks. github-dark reads well on the
// dark code surface used in both light and dark app themes.
import 'highlight.js/styles/github-dark.css'

/** A minimal mdast node (only the fields this file touches). */
interface MdNode {
  type: string
  value?: string
  children?: MdNode[]
}

/**
 * Render a single newline as a `<br>` (a hard break) instead of collapsing it to
 * a space. Standard Markdown treats a lone `\n` as a *soft* break (whitespace),
 * so model output like `1\n2\n3` would otherwise render as `1 2 3`. This mirrors
 * `remark-breaks` (and how ChatGPT/Claude render chat prose) without the extra
 * dependency. Only `text` nodes are split, so fenced/inline code — which are
 * `code`/`inlineCode` nodes, not `text` — keep their newlines untouched.
 */
function remarkSoftBreaks() {
  const walk = (node: MdNode) => {
    if (!node.children) return
    const out: MdNode[] = []
    for (const child of node.children) {
      if (child.type === 'text' && child.value && child.value.includes('\n')) {
        const segments = child.value.split('\n')
        segments.forEach((seg, i) => {
          if (i > 0) out.push({ type: 'break' })
          if (seg) out.push({ type: 'text', value: seg })
        })
      } else {
        walk(child)
        out.push(child)
      }
    }
    node.children = out
  }
  return (tree: MdNode) => walk(tree)
}

/** A tool invocation surfaced over the stream / persisted on the message. */
/**
 * Per-message copy affordance.
 *
 * Revealed on hover (and on keyboard focus, so it is not mouse-only) to keep a
 * long transcript from turning into a column of icons. Nothing is rendered when
 * the message has no copyable prose — a button that copies an empty string
 * looks like it worked and didn't.
 */
function MessageCopyButton({ text }: { text: string }) {
  if (!text) return null
  return (
    <CopyButton
      value={text}
      minimal
      label="Copy message"
      className="rounded p-1 text-muted-foreground opacity-0 transition-opacity focus-visible:opacity-100 group-hover:opacity-100 [&_svg]:size-3"
    />
  )
}

function formatAttachmentSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

function ChatAttachments({
  attachments,
  onRemove,
  contentBase,
}: {
  attachments: ChatAttachment[]
  onRemove?: (id: string) => void
  contentBase?: string
}) {
  if (attachments.length === 0) return null
  return (
    <div className="flex max-w-full flex-wrap gap-2">
      {attachments.map((attachment) => {
        const contentUrl = contentBase
          ? `${contentBase}/${encodeURIComponent(attachment.id)}?name=${encodeURIComponent(attachment.name)}`
          : undefined
        const previewUrl =
          attachment.preview_url ??
          (attachment.is_image ? contentUrl : undefined)
        const contents = (
          <>
            {attachment.is_image && previewUrl ? (
              <img
                src={previewUrl}
                alt=""
                className="size-10 shrink-0 rounded-md object-cover"
              />
            ) : attachment.is_image ? (
              <ImageIcon className="size-5 shrink-0 text-muted-foreground" />
            ) : (
              <FileText className="size-5 shrink-0 text-muted-foreground" />
            )}
            <div className="min-w-0 pr-1">
              <p className="truncate text-xs font-medium">{attachment.name}</p>
              <p className="text-[10px] text-muted-foreground">
                {formatAttachmentSize(attachment.size_bytes)}
              </p>
            </div>
            {onRemove && (
              <button
                type="button"
                onClick={() => onRemove(attachment.id)}
                className="absolute right-1 top-1 rounded-full bg-background/90 p-0.5 text-muted-foreground opacity-0 shadow-sm transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover/attachment:opacity-100"
                aria-label={`Remove ${attachment.name}`}
              >
                <X className="size-3" />
              </button>
            )}
          </>
        )
        const className =
          'group/attachment relative flex max-w-56 items-center gap-2 overflow-hidden rounded-lg border border-border/70 bg-background/80 p-1.5 text-left text-foreground'
        return contentUrl && !onRemove ? (
          <a
            key={attachment.id}
            href={contentUrl}
            target="_blank"
            rel="noreferrer"
            className={`${className} transition-colors hover:bg-muted/70`}
            aria-label={`Open ${attachment.name}`}
          >
            {contents}
          </a>
        ) : (
          <div key={attachment.id} className={className}>
            {contents}
          </div>
        )
      })}
    </div>
  )
}

/**
 * Maps a `getConversation` response into the panel's `ChatMessage[]` state,
 * including a still-unresolved permission request (ADR-038 Phase 2) as a live,
 * answerable card rather than the inert "asked" text alone — used both on
 * initial load and by `pollForReply`'s fallback refetch.
 */
function mapConversationDetail(detail: {
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
}): ChatMessage[] {
  const mapped: ChatMessage[] = (detail.messages ?? []).map((m) => {
    const rawParts = m.parts
    return {
      server_cursor: m.cursor,
      role: m.role,
      content: m.content,
      created_at: m.created_at,
      tools: m.tools ?? undefined,
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

type ConversationHistoryPage = {
  has_more: boolean
  next_before?: string | null
}

type PaginatedConversationDetail = ConversationDetailResponse & {
  page?: ConversationHistoryPage
}

function conversationHistoryPage(detail: {
  page?: ConversationHistoryPage
}): ConversationHistoryPage {
  return detail.page ?? { has_more: false, next_before: null }
}

/**
 * Pop the trailing optimistic assistant turn if it never received anything.
 * Checking `content` alone isn't enough: a pending permission card lives in
 * `parts`, not `content` — dropping the turn on a dead connection would
 * silently discard a still-answerable question with no way to resolve it
 * (ADR-038 Phase 2).
 */
type SetMessages = React.Dispatch<React.SetStateAction<ChatMessage[]>>

export interface ChatFailure {
  code: string
  title: string
  detail: string
  retryable: boolean
}

type SetChatFailure = React.Dispatch<React.SetStateAction<ChatFailure | null>>

const UNKNOWN_CHAT_FAILURE: ChatFailure = {
  code: 'harness_failed',
  title: 'AI harness failed',
  detail:
    'The selected harness stopped before Temps received a reply. Retry once; if it repeats, check its authentication, selected model, and the server logs.',
  retryable: true,
}

function localChatFailure(
  title: string,
  detail: string,
  code = 'chat_request_failed',
  retryable = true
): ChatFailure {
  return { code, title, detail, retryable }
}

function isSafeFailureText(value: unknown, maxLength: number): value is string {
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

function dropEmptyAssistantTurn(setMessages: SetMessages) {
  setMessages((m) => {
    const last = m[m.length - 1]
    return last?.role === 'assistant' &&
      last.content === '' &&
      !(last.parts && last.parts.length > 0)
      ? m.slice(0, -1)
      : m
  })
}

function dropOptimisticTurn(setMessages: SetMessages, turnId: string) {
  setMessages((messages) =>
    messages.filter((message) => message.client_turn_id !== turnId)
  )
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
function applyWireEvent(
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
      setMessages((m) => {
        const copy = [...m]
        const last = copy[copy.length - 1]
        if (last?.role === 'assistant') {
          const tool: ToolCall = {
            id: t.id,
            name: t.name,
            arguments: t.arguments,
            result: undefined,
          }
          copy[copy.length - 1] = {
            ...last,
            tools: [...(last.tools ?? []), tool],
            parts: [...(last.parts ?? []), { type: 'tool', tool }],
          }
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
      setMessages((m) => {
        const copy = [...m]
        const last = copy[copy.length - 1]
        if (last?.role === 'assistant') {
          copy[copy.length - 1] = {
            ...last,
            tools: (last.tools ?? []).map((tool) =>
              tool.id === t.id ? { ...tool, result: t.content } : tool
            ),
            parts: (last.parts ?? []).map((part) =>
              part.type === 'tool' && part.tool.id === t.id
                ? { type: 'tool', tool: { ...part.tool, result: t.content } }
                : part
            ),
          }
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
 * The plain text a "copy" on this message should yield.
 *
 * For an assistant turn that's the prose only — the tool cards are UI around
 * the work, not something anyone wants pasted into an issue or a commit
 * message. A turn that ran tools but never produced prose has nothing useful to
 * copy, so it returns empty and the caller hides the button rather than
 * offering one that silently copies nothing.
 */
function messageCopyText(m: ChatMessage): string {
  if (m.role === 'user') return m.content
  return assistantParts(m)
    .filter((p): p is { type: 'text'; text: string } => p.type === 'text')
    .map((p) => p.text)
    .join('\n')
    .trim()
}

/**
 * Human label for a tool card — what the tool actually did. For the `temps` and
 * `temps_write` virtual CLIs that's the command it ran (e.g.
 * `traces get_trace --trace_id …`, or `trigger_project_pipeline --environment_id 8`),
 * which is far more useful than several identical "temps"/"temps_write" rows.
 * Falls back to the tool name for other tools or unparsable args.
 */
export function toolLabel(tool: ToolCall): string {
  // MCP clients qualify tool names as `mcp__<server>__<tool>`. The chat wire
  // persists that provider-native name so it can be inspected later, but the
  // compact row should describe the operation rather than the transport.
  const qualifiedNameParts = tool.name.split('__')
  const baseName =
    qualifiedNameParts[qualifiedNameParts.length - 1] || tool.name

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
  if (baseName === 'Bash') {
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
  if (baseName === 'Read' || baseName === 'Edit' || baseName === 'Write') {
    try {
      const args = JSON.parse(tool.arguments) as Record<string, unknown>
      const path = args.file_path ?? args.path ?? args.target_file
      if (typeof path === 'string' && path.trim()) {
        return `${baseName} · ${path.trim()}`
      }
    } catch {
      /* fall through to the tool name */
    }
  }
  return tool.name
}

const toolBlockClasses =
  'max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-background p-2 font-mono text-[11px]'

/**
 * Render a tool's arguments/result. JSON is syntax-highlighted via the same
 * `rehype-highlight` pipeline the assistant's code blocks use (so it matches the
 * rest of the chat); non-JSON text (CLI `--help` output, errors) falls back to a
 * plain preformatted block. Height-capped with its own scroll.
 */
function ToolBlock({ value }: { value: string }) {
  let json: string
  try {
    json = JSON.stringify(JSON.parse(value), null, 2)
  } catch {
    return <pre className={toolBlockClasses}>{value}</pre>
  }
  return (
    <div
      className={cn(
        proseClasses,
        'scrollbar-thin max-h-48 overflow-auto [&_pre]:my-0 [&_pre]:text-[11px]'
      )}
    >
      <ReactMarkdown
        rehypePlugins={[
          [rehypeHighlight, { detect: true, ignoreMissing: true }],
        ]}
        components={markdownComponents}
      >
        {`\`\`\`json\n${json}\n\`\`\``}
      </ReactMarkdown>
    </div>
  )
}

/** A collapsible card for one tool invocation + its result. */
function ToolCard({ tool }: { tool: ToolCall }) {
  const [open, setOpen] = useState(false)
  const running = tool.result === undefined
  const label = toolLabel(tool)
  return (
    <div className="min-w-0 overflow-hidden rounded-lg border bg-muted/40 text-xs">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex w-full min-w-0 items-center gap-2 px-2.5 py-1.5 text-left transition-colors hover:bg-muted/70"
        aria-expanded={open}
      >
        <Wrench className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        <span
          className="min-w-0 flex-1 truncate font-mono text-[11px] font-medium"
          title={label}
        >
          {label}
        </span>
        {running && <ActivityIndicator compact label="Running" />}
        {open ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        )}
      </button>
      {open && (
        <div className="min-w-0 space-y-2 border-t px-2.5 py-2">
          <div className="min-w-0 space-y-1">
            <div className="font-medium text-muted-foreground">Arguments</div>
            <ToolBlock value={tool.arguments} />
          </div>
          {!running && (
            <div className="min-w-0 space-y-1">
              <div className="font-medium text-muted-foreground">Result</div>
              <ToolBlock value={tool.result ?? ''} />
            </div>
          )}
        </div>
      )}
    </div>
  )
}

/** Deterministic native presentations for trusted read operations. Unknown
 * operations keep the transparent generic tool card instead of guessing. */
function ReadToolResultCard({ tool }: { tool: ToolCall }) {
  const projectCollection = projectCollectionFromTool(tool)
  if (!projectCollection) return <ToolCard tool={tool} />

  return (
    <div className="min-w-0 overflow-hidden rounded-lg border border-border bg-background text-xs">
      <GeneratedProjectCollection
        presentation={projectCollection}
        framed={false}
      />
      <details className="group border-t">
        <summary className="flex cursor-pointer list-none items-center gap-1.5 px-3 py-2 text-[11px] font-medium text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground sm:px-4">
          <ChevronRight className="size-3.5 transition-transform group-open:rotate-90" />
          Request details
        </summary>
        <div className="min-w-0 space-y-2 border-t px-3 py-2.5 sm:px-4">
          <div className="min-w-0 space-y-1">
            <div className="font-medium text-muted-foreground">Arguments</div>
            <ToolBlock value={tool.arguments} />
          </div>
          <div className="min-w-0 space-y-1">
            <div className="font-medium text-muted-foreground">Result</div>
            <ToolBlock value={tool.result ?? ''} />
          </div>
        </div>
      </details>
    </div>
  )
}

/** The proposal payload a `temps_write` tool result carries (JSON string). */
interface Proposal {
  action_id: string
  operation: string
  method: string
  summary: string
}

/** Parse a `temps_write` tool result into a proposal, or null when it's help /
 *  validation text (rendered as a plain tool card instead). */
function parseProposal(result?: string | null): Proposal | null {
  if (!result) return null
  try {
    const o = JSON.parse(result) as Partial<Proposal> & { status?: string }
    if (o && o.status === 'proposed' && o.action_id && o.operation) {
      return {
        action_id: String(o.action_id),
        operation: String(o.operation),
        method: String(o.method ?? ''),
        summary: String(o.summary ?? ''),
      }
    }
  } catch {
    /* not a proposal payload */
  }
  return null
}

/** Compose a chat message that hands a failed write action back to the AI with
 *  everything it needs to diagnose and re-propose: the operation, the exact
 *  params sent (already redacted server-side), and the error. */
function buildFixMessage(
  op: string,
  method: string,
  params: string | null,
  error: string | null
): string {
  return [
    'A write action I confirmed just FAILED. Diagnose the cause from the error ' +
      'below and propose a corrected action (look up any wrong/missing ids with ' +
      'the read tool first). Do not claim success.',
    '',
    `- Operation: \`${op}\``,
    method ? `- Method: \`${method}\`` : '',
    params && params !== '{}' ? `- Params sent: ${params}` : '',
    error ? `- Error: ${error}` : '',
  ]
    .filter(Boolean)
    .join('\n')
}

const ACTION_STATUS: Record<string, { label: string; cls: string }> = {
  proposed: {
    label: 'Awaiting your confirmation',
    cls: 'text-amber-600 dark:text-amber-400',
  },
  executing: { label: 'Running…', cls: 'text-muted-foreground' },
  executed: { label: 'Executed', cls: 'text-green-600 dark:text-green-400' },
  failed: { label: 'Failed', cls: 'text-destructive' },
  rejected: { label: 'Rejected', cls: 'text-muted-foreground' },
  expired: { label: 'Expired', cls: 'text-muted-foreground' },
}

/**
 * A write/modify/delete the AI has *proposed* — never executed. This card is the
 * human gate: Confirm replays the mutation server-side (permission-checked +
 * audited), Reject discards it. On mount it reconciles the live status from the
 * API, so a reloaded chat shows executed/rejected instead of a stale prompt.
 */
function PendingActionCard({
  pendingActionBasePath,
  tool,
  onFix,
  onStatusChange,
}: {
  pendingActionBasePath: string
  tool: ToolCall
  /** Send a follow-up chat message (used by "Fix with AI" on failure). */
  onFix?: (text: string) => void
  /** Reconcile a standalone recovery card with its parent-owned snapshot. */
  onStatusChange?: (status: string) => void
}) {
  const proposal = parseProposal(tool.result)
  const committedApplicationProjects =
    projectCollectionFromApplicationProjectWrite(tool)
  const actionId = proposal?.action_id
  const [status, setStatus] = useState('proposed')
  const [busy, setBusy] = useState<'confirm' | 'reject' | null>(null)
  const [result, setResult] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [createdAt, setCreatedAt] = useState<string | null>(null)
  // The exact request params/body that will be sent, redacted server-side
  // (value/secret/password/token/key → ***). Shown so the user can review what
  // the action will actually do before confirming.
  const [params, setParams] = useState<string | null>(null)
  const [open, setOpen] = useState(false)
  const [requestOpen, setRequestOpen] = useState(false)
  const onStatusChangeRef = useRef(onStatusChange)

  useEffect(() => {
    onStatusChangeRef.current = onStatusChange
  }, [onStatusChange])

  // Reconcile the live status once the action id is known (covers reloads).
  useEffect(() => {
    if (!actionId) return
    let cancelled = false
    fetch(`${pendingActionBasePath}/${actionId}`, {
      credentials: 'include',
    })
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => {
        if (cancelled || !d) return
        if (typeof d.status === 'string') {
          setStatus(d.status)
          onStatusChangeRef.current?.(d.status)
        }
        if (d.result != null) setResult(JSON.stringify(d.result))
        if (typeof d.error === 'string') setError(d.error)
        if (d.params != null) setParams(JSON.stringify(d.params))
        if (typeof d.created_at === 'string') setCreatedAt(d.created_at)
      })
      .catch(() => {
        /* status reconcile is best-effort */
      })
    return () => {
      cancelled = true
    }
  }, [pendingActionBasePath, actionId])

  if (committedApplicationProjects) {
    return (
      <div className="min-w-0 overflow-hidden rounded-lg border border-green-500/30 bg-green-500/5 text-xs">
        <GeneratedProjectCollection
          title="Application projects"
          presentation={committedApplicationProjects}
          framed={false}
        />
        <details className="group border-t border-green-500/20">
          <summary className="flex cursor-pointer list-none items-center gap-1.5 px-3 py-2 text-[11px] font-medium text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground sm:px-4">
            <ChevronRight className="size-3.5 transition-transform group-open:rotate-90" />
            Operation details
          </summary>
          <div className="min-w-0 space-y-2 border-t border-green-500/20 px-3 py-2.5 sm:px-4">
            <div className="font-medium text-green-600 dark:text-green-400">
              Project created and attached to the persistent workspace
            </div>
            <ToolBlock value={tool.result ?? ''} />
          </div>
        </details>
      </div>
    )
  }

  // Still streaming the proposal, or not a proposal at all (help/validation
  // text) — fall back to the ordinary tool card.
  if (tool.result === undefined || !proposal) {
    return <ToolCard tool={tool} />
  }

  const act = async (kind: 'confirm' | 'reject') => {
    setBusy(kind)
    setError(null)
    try {
      const r = await fetch(
        `${pendingActionBasePath}/${proposal.action_id}/${kind}`,
        {
          method: 'POST',
          credentials: 'include',
          headers: { 'Content-Type': 'application/json' },
        }
      )
      const d = await r.json().catch(() => null)
      if (!r.ok) {
        setError(
          (d as { detail?: string } | null)?.detail ||
            `Could not ${kind} the action.`
        )
      } else if (d) {
        if (typeof d.status === 'string') {
          setStatus(d.status)
          onStatusChangeRef.current?.(d.status)
        }
        if (d.result != null) setResult(JSON.stringify(d.result))
        if (typeof d.error === 'string') setError(d.error)
        if (typeof d.created_at === 'string') setCreatedAt(d.created_at)
      }
    } catch {
      setError(`Could not ${kind} the action.`)
    } finally {
      setBusy(null)
    }
  }

  const st = ACTION_STATUS[status] ?? ACTION_STATUS.proposed
  const pending = status === 'proposed'
  const serviceProposal =
    proposal.operation === 'create_service'
      ? serviceProposalViewModel(params)
      : null
  const serviceId =
    proposal.operation === 'create_service' && status === 'executed'
      ? createdServiceId(result)
      : null
  const deploymentProposal =
    proposal.operation === 'trigger_project_pipeline' ||
    proposal.operation === 'deploy_application_workspace_project'
  return (
    <div className="min-w-0 overflow-hidden rounded-lg border border-amber-500/30 bg-amber-500/5 text-xs">
      {serviceProposal ? (
        <GeneratedServiceProposal
          proposal={serviceProposal}
          summary={proposal.summary}
          statusLabel={st.label}
          statusClassName={st.cls}
          serviceId={serviceId}
        />
      ) : deploymentProposal ? (
        <GeneratedDeploymentCard
          paramsJson={params}
          resultJson={result}
          actionStatus={status}
          createdAt={createdAt}
          summary={proposal.summary}
          statusLabel={st.label}
          statusClassName={st.cls}
        />
      ) : (
        <div className="flex min-w-0 items-start gap-2 px-2.5 py-2">
          <ShieldCheck className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
          <div className="min-w-0 flex-1 space-y-0.5">
            <div className="flex items-center gap-1.5">
              <span className="rounded bg-muted px-1 py-0.5 font-mono text-[10px] font-semibold uppercase">
                {proposal.method}
              </span>
              <span className="min-w-0 truncate font-mono text-[11px] font-medium">
                {proposal.operation}
              </span>
            </div>
            {proposal.summary && (
              <div className="text-muted-foreground">{proposal.summary}</div>
            )}
            <div className={cn('text-[11px] font-medium', st.cls)}>
              {st.label}
            </div>
          </div>
        </div>
      )}
      {params && params !== '{}' && (
        <div className="min-w-0 space-y-1 border-t border-amber-500/20 px-2.5 py-2">
          {serviceProposal || deploymentProposal ? (
            <>
              <button
                type="button"
                onClick={() => setRequestOpen((value) => !value)}
                aria-expanded={requestOpen}
                className="flex items-center gap-1 font-medium text-muted-foreground hover:text-foreground"
              >
                {requestOpen ? (
                  <ChevronDown className="h-3.5 w-3.5" />
                ) : (
                  <ChevronRight className="h-3.5 w-3.5" />
                )}
                Request details
              </button>
              {requestOpen && <ToolBlock value={params} />}
            </>
          ) : (
            <>
              <div className="font-medium text-muted-foreground">
                {pending ? 'Will send' : 'Sent'}
              </div>
              <ToolBlock value={params} />
            </>
          )}
        </div>
      )}
      {pending ? (
        <div className="flex items-center gap-2 border-t border-amber-500/20 px-2.5 py-2">
          <Button
            type="button"
            size="sm"
            className="h-7 gap-1 px-2 text-xs"
            disabled={busy !== null}
            onClick={() => act('confirm')}
          >
            {busy === 'confirm' ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Check className="h-3.5 w-3.5" />
            )}
            Confirm &amp; run
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-7 gap-1 px-2 text-xs"
            disabled={busy !== null}
            onClick={() => act('reject')}
          >
            {busy === 'reject' ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <X className="h-3.5 w-3.5" />
            )}
            Reject
          </Button>
        </div>
      ) : result || error ? (
        <div className="space-y-1 border-t border-amber-500/20 px-2.5 py-2">
          <button
            type="button"
            onClick={() => setOpen((o) => !o)}
            className="flex items-center gap-1 font-medium text-muted-foreground hover:text-foreground"
          >
            {open ? (
              <ChevronDown className="h-3.5 w-3.5" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5" />
            )}
            {error ? 'Error' : 'Result'}
          </button>
          {open &&
            (error ? (
              <pre className={toolBlockClasses}>{error}</pre>
            ) : (
              <ToolBlock value={result ?? ''} />
            ))}
          {status === 'failed' && onFix && (
            <Button
              type="button"
              size="sm"
              variant="outline"
              className="mt-1 h-7 gap-1 px-2 text-xs"
              onClick={() =>
                onFix(
                  buildFixMessage(
                    proposal.operation,
                    proposal.method,
                    params,
                    error
                  )
                )
              }
            >
              <Sparkles className="h-3.5 w-3.5" />
              Fix with AI
            </Button>
          )}
        </div>
      ) : null}
    </div>
  )
}

/** One step of a multi-step plan proposal. */
interface PlanStep {
  action_id: string
  operation: string
  method: string
  summary: string
  step: number
}

interface PlanProposal {
  plan_id: string | null
  steps: PlanStep[]
}

/** Parse a `temps_write` tool result into a plan proposal, or null when it's a
 *  single action / help text (handled elsewhere). */
function parsePlanProposal(result?: string | null): PlanProposal | null {
  if (!result) return null
  try {
    const o = JSON.parse(result) as {
      status?: string
      plan_id?: string
      steps?: Array<Partial<PlanStep>>
    }
    if (o?.status === 'proposed_plan' && Array.isArray(o.steps)) {
      const steps = o.steps
        .map((s, i) => ({
          action_id: String(s.action_id ?? ''),
          operation: String(s.operation ?? ''),
          method: String(s.method ?? ''),
          summary: String(s.summary ?? ''),
          step: Number(s.step ?? i + 1),
        }))
        .filter((s) => s.action_id)
      if (steps.length > 0) {
        return { plan_id: o.plan_id ? String(o.plan_id) : null, steps }
      }
    }
  } catch {
    /* not a plan payload */
  }
  return null
}

interface StepState {
  status: string
  result?: string | null
  error?: string | null
  params?: string | null
}

/**
 * A multi-step *plan* the AI proposed (chained actions, e.g. "raise resources
 * then redeploy"). Every step is shown up front, but steps run ONE AT A TIME in
 * order: only the next un-run step is actionable, confirming it replays that one
 * mutation server-side (permission-checked + audited), and a failed or rejected
 * step halts the rest. Each step is its own pending-action row; this card just
 * groups and sequences them.
 */
function PlanActionCard({
  pendingActionBasePath,
  plan,
  onFix,
}: {
  pendingActionBasePath: string
  plan: PlanProposal
  /** Send a follow-up chat message (used by "Fix with AI" on a failed step). */
  onFix?: (text: string) => void
}) {
  const [states, setStates] = useState<Record<string, StepState>>(() =>
    Object.fromEntries(
      plan.steps.map((s) => [s.action_id, { status: 'proposed' } as StepState])
    )
  )
  const [busy, setBusy] = useState<string | null>(null)
  const [openId, setOpenId] = useState<string | null>(null)

  const fetchAll = useCallback(() => {
    let cancelled = false
    void Promise.all(
      plan.steps.map((s) =>
        fetch(`${pendingActionBasePath}/${s.action_id}`, {
          credentials: 'include',
        })
          .then((r) => (r.ok ? r.json() : null))
          .then((d) => [s.action_id, d] as const)
          .catch(() => [s.action_id, null] as const)
      )
    ).then((pairs) => {
      if (cancelled) return
      setStates((prev) => {
        const next = { ...prev }
        for (const [id, d] of pairs) {
          if (d)
            next[id] = {
              status: typeof d.status === 'string' ? d.status : 'proposed',
              result: d.result != null ? JSON.stringify(d.result) : null,
              error: typeof d.error === 'string' ? d.error : null,
              params: d.params != null ? JSON.stringify(d.params) : null,
            }
        }
        return next
      })
    })
    return () => {
      cancelled = true
    }
  }, [pendingActionBasePath, plan])

  useEffect(() => fetchAll(), [fetchAll])

  const statuses = plan.steps.map(
    (s) => states[s.action_id]?.status ?? 'proposed'
  )
  // The next actionable step: the first still-'proposed' step whose every
  // predecessor has 'executed'. If a predecessor failed/rejected/skipped, the
  // plan is halted and nothing is actionable.
  let actionableIdx = -1
  for (let i = 0; i < plan.steps.length; i++) {
    if (statuses[i] === 'proposed') {
      if (statuses.slice(0, i).every((st) => st === 'executed'))
        actionableIdx = i
      break
    }
  }

  const act = async (actionId: string, kind: 'confirm' | 'reject') => {
    setBusy(actionId)
    try {
      const r = await fetch(`${pendingActionBasePath}/${actionId}/${kind}`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
      })
      const d = await r.json().catch(() => null)
      setStates((prev) => ({
        ...prev,
        [actionId]: {
          status:
            typeof d?.status === 'string'
              ? d.status
              : (prev[actionId]?.status ?? 'proposed'),
          result:
            d?.result != null
              ? JSON.stringify(d.result)
              : prev[actionId]?.result,
          error: !r.ok
            ? ((d as { detail?: string } | null)?.detail ??
              `Could not ${kind} this step.`)
            : typeof d?.error === 'string'
              ? d.error
              : prev[actionId]?.error,
          params: prev[actionId]?.params,
        },
      }))
    } catch {
      setStates((prev) => ({
        ...prev,
        [actionId]: {
          ...(prev[actionId] ?? { status: 'proposed' }),
          error: `Could not ${kind} this step.`,
        },
      }))
    } finally {
      setBusy(null)
      // Refetch so a failed/rejected step's cascade (later steps → skipped) shows.
      fetchAll()
    }
  }

  const doneCount = statuses.filter((st) => st === 'executed').length
  const halted =
    actionableIdx === -1 &&
    statuses.some((st) => ['failed', 'rejected', 'skipped'].includes(st))

  return (
    <div className="min-w-0 overflow-hidden rounded-lg border border-amber-500/30 bg-amber-500/5 text-xs">
      <div className="flex items-center gap-2 border-b border-amber-500/20 px-2.5 py-2">
        <ShieldCheck className="h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
        <span className="font-medium">Multi-step plan</span>
        <span className="text-muted-foreground">
          {doneCount}/{plan.steps.length} done
          {halted ? ' · halted' : ''}
        </span>
      </div>
      <ol className="min-w-0">
        {plan.steps.map((s, i) => {
          const stt = states[s.action_id] ?? { status: 'proposed' }
          const meta = ACTION_STATUS[stt.status] ?? ACTION_STATUS.proposed
          const isActionable = i === actionableIdx
          const waiting = stt.status === 'proposed' && !isActionable
          const isOpen = openId === s.action_id
          return (
            <li
              key={s.action_id}
              className={cn(
                'min-w-0 border-t border-amber-500/20 px-2.5 py-2 first:border-t-0',
                waiting && 'opacity-60'
              )}
            >
              <div className="flex min-w-0 items-start gap-2">
                <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-full bg-muted font-mono text-[10px] font-semibold">
                  {s.step}
                </span>
                <div className="min-w-0 flex-1 space-y-0.5">
                  <div className="flex items-center gap-1.5">
                    <span className="rounded bg-muted px-1 py-0.5 font-mono text-[10px] font-semibold uppercase">
                      {s.method}
                    </span>
                    <span className="min-w-0 truncate font-mono text-[11px] font-medium">
                      {s.operation}
                    </span>
                  </div>
                  {s.summary && (
                    <div className="text-muted-foreground">{s.summary}</div>
                  )}
                  <div className={cn('text-[11px] font-medium', meta.cls)}>
                    {waiting ? 'Waiting for earlier steps' : meta.label}
                  </div>
                  {(stt.params && stt.params !== '{}') ||
                  stt.result ||
                  stt.error ? (
                    <button
                      type="button"
                      onClick={() => setOpenId(isOpen ? null : s.action_id)}
                      className="mt-0.5 flex items-center gap-1 font-medium text-muted-foreground hover:text-foreground"
                    >
                      {isOpen ? (
                        <ChevronDown className="h-3.5 w-3.5" />
                      ) : (
                        <ChevronRight className="h-3.5 w-3.5" />
                      )}
                      {stt.error ? 'Error' : stt.result ? 'Result' : 'Details'}
                    </button>
                  ) : null}
                  {isOpen && (
                    <div className="space-y-1 pt-1">
                      {stt.params && stt.params !== '{}' && (
                        <>
                          <div className="font-medium text-muted-foreground">
                            {isActionable ? 'Will send' : 'Sent'}
                          </div>
                          <ToolBlock value={stt.params} />
                        </>
                      )}
                      {stt.error ? (
                        <pre className={toolBlockClasses}>{stt.error}</pre>
                      ) : stt.result ? (
                        <ToolBlock value={stt.result} />
                      ) : null}
                    </div>
                  )}
                </div>
              </div>
              {isActionable && (
                <div className="mt-2 flex items-center gap-2 pl-6">
                  <Button
                    type="button"
                    size="sm"
                    className="h-7 gap-1 px-2 text-xs"
                    disabled={busy !== null}
                    onClick={() => act(s.action_id, 'confirm')}
                  >
                    {busy === s.action_id ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <Check className="h-3.5 w-3.5" />
                    )}
                    Confirm step {s.step}
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    className="h-7 gap-1 px-2 text-xs"
                    disabled={busy !== null}
                    onClick={() => act(s.action_id, 'reject')}
                  >
                    <X className="h-3.5 w-3.5" />
                    Reject
                  </Button>
                </div>
              )}
              {stt.status === 'failed' && onFix && (
                <div className="mt-2 pl-6">
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    className="h-7 gap-1 px-2 text-xs"
                    onClick={() =>
                      onFix(
                        buildFixMessage(
                          s.operation,
                          s.method,
                          stt.params ?? null,
                          stt.error ?? null
                        )
                      )
                    }
                  >
                    <Sparkles className="h-3.5 w-3.5" />
                    Fix with AI
                  </Button>
                </div>
              )}
            </li>
          )
        })}
      </ol>
    </div>
  )
}

/** Route a `temps_write` tool part to the plan card or the single-action card. */
function WriteProposalCard({
  pendingActionBasePath,
  tool,
  onFix,
}: {
  pendingActionBasePath: string
  tool: ToolCall
  onFix?: (text: string) => void
}) {
  const plan = parsePlanProposal(tool.result)
  if (plan)
    return (
      <PlanActionCard
        pendingActionBasePath={pendingActionBasePath}
        plan={plan}
        onFix={onFix}
      />
    )
  return (
    <PendingActionCard
      pendingActionBasePath={pendingActionBasePath}
      tool={tool}
      onFix={onFix}
    />
  )
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

function InteractiveToolsNotice({
  provider,
  contextType,
  permissionMode,
}: {
  provider: string
  contextType: string
  permissionMode: string | null
}) {
  // null = still loading / unknown.
  const [supported, setSupported] = useState<boolean | null>(null)
  // `interactive_bridge_status` from the extended provider status response.
  // Not in the generated types yet, so we read it defensively.
  const [bridgeStatus, setBridgeStatus] = useState<string | null | undefined>(
    undefined
  )

  useEffect(() => {
    if (contextType === 'application') return
    let cancelled = false
    getAiProviderStatus()
      .then(({ data }) => {
        if (!cancelled && data) {
          setSupported(data.supports_interactive_tools)
          // Read defensively: field is present on the backend but the
          // generated client type hasn't been updated yet.
          const extended = data as typeof data & {
            interactive_bridge_status?: string | null
          }
          setBridgeStatus(extended.interactive_bridge_status ?? null)
        }
      })
      .catch(() => {
        /* leave unknown — just don't show the notice */
      })
    return () => {
      cancelled = true
    }
  }, [contextType])

  if (contextType === 'application') {
    const notice = applicationHarnessPermissionNotice(provider, permissionMode)
    if (!notice) return null
    return (
      <div className="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-2.5 py-1.5 text-xs text-amber-700 dark:text-amber-400">
        <Info className="h-3.5 w-3.5 shrink-0 translate-y-px" />
        <span className="min-w-0 flex-1">{notice}</span>
      </div>
    )
  }

  // Still loading.
  if (supported === null && bridgeStatus === undefined) return null

  // Gateway conversations use native provider APIs and do not depend on the
  // instance-level host CLI preference reported by this endpoint.
  if (provider === 'gateway' || provider.startsWith('gateway_key:')) return null

  if (bridgeStatus === 'healthy') return null

  return (
    <div className="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-2.5 py-1.5 text-xs text-amber-700 dark:text-amber-400">
      <Info className="h-3.5 w-3.5 shrink-0 translate-y-px" />
      <span className="min-w-0 flex-1">
        {bridgeStatus === 'unavailable'
          ? 'This host CLI is selected but is not authenticated right now. Restore its host login, then reload this chat.'
          : 'This CLI uses its provider-native permission mode. Inline Temps approval cards are available only when that CLI exposes an interactive permission bridge.'}
      </span>
    </div>
  )
}

interface DebugChatPanelProps {
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

function providerDisplayName(provider: string): string {
  if (provider.startsWith('gateway_key:')) return 'AI Gateway'
  switch (provider) {
    case 'gateway':
      return 'AI Gateway'
    case 'claude_cli':
      return 'Claude Code'
    case 'codex_cli':
      return 'Codex'
    case 'opencode':
      return 'OpenCode'
    default:
      return provider
  }
}

/** The distance from the bottom that still counts as following the transcript. */
const CHAT_SCROLL_BOTTOM_THRESHOLD_PX = 72

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

const proseClasses =
  'prose prose-sm dark:prose-invert max-w-none prose-pre:bg-[#0d1117] prose-pre:text-xs prose-pre:border-0 prose-pre:overflow-x-auto prose-pre:rounded-lg prose-code:before:content-none prose-code:after:content-none prose-p:my-1.5 prose-headings:my-2 prose-ul:my-1.5 prose-ul:list-disc prose-ul:pl-5 prose-ol:my-1.5 prose-ol:list-decimal prose-ol:pl-5 prose-li:my-0.5 prose-li:marker:text-foreground/60 prose-hr:my-3 prose-hr:border-border prose-table:text-xs prose-th:px-2 prose-th:py-1 prose-td:px-2 prose-td:py-1'

const pixelDelays = [90, 180, 270, 0, 90, 180, 90, 180, 270]

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
 * The chat's working state is intentionally a small, utilitarian instrument:
 * a pixel wave and a shimmer rather than a full-screen spinner. It works on
 * either console theme because it only uses semantic foreground tokens.
 */
function ActivityIndicator({
  compact = false,
  label,
  startedAt,
}: {
  compact?: boolean
  label: string
  /** Durable server timestamp for the active turn. */
  startedAt?: string | null
}) {
  const [nowMs, setNowMs] = useState(() => Date.now())

  useEffect(() => {
    if (compact) return
    const timer = window.setInterval(() => {
      setNowMs(Date.now())
    }, 100)
    return () => window.clearInterval(timer)
  }, [compact])

  const deciseconds = serverElapsedDeciseconds(startedAt, nowMs)
  const elapsed = deciseconds / 10
  const elapsedLabel =
    elapsed < 60
      ? `${elapsed.toFixed(1)}s`
      : `${Math.floor(elapsed / 60)}m ${(elapsed % 60).toFixed(1)}s`

  return (
    <span
      className={cn(
        'inline-flex items-center',
        compact ? 'gap-1.5' : 'gap-2.5 py-1'
      )}
      role="status"
      aria-label={label}
    >
      <span
        aria-hidden
        className="grid shrink-0 grid-cols-[repeat(3,4px)] gap-[1.5px]"
      >
        {pixelDelays.map((delay, index) => (
          <span
            key={index}
            className="ai-activity-pixel h-[4px] w-[4px] rounded-[1px] bg-foreground"
            style={{ animationDelay: `${delay}ms` }}
          />
        ))}
      </span>
      {!compact && (
        <span className="ai-activity-shimmer bg-gradient-to-r from-muted-foreground via-foreground to-muted-foreground bg-[length:200%_100%] bg-clip-text text-[13px] font-medium text-transparent">
          {label}
        </span>
      )}
      {!compact && (
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
          {elapsedLabel}
        </span>
      )}
    </span>
  )
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

/** Inline server-owned activity state for an assistant turn. */
function TurnActivity({
  label,
  startedAt,
}: {
  label: string
  startedAt?: string | null
}) {
  return <ActivityIndicator label={label} startedAt={startedAt} />
}

/** Open links (including `remark-gfm` autolinked bare URLs) in a new tab, styled
 *  as links. `rel="noopener noreferrer"` so the opened page can't access us. */
const markdownComponents: Components = {
  // Give horizontally-scrolling code blocks a thin, subtle scrollbar instead of
  // the chunky default OS bar over the dark code surface.
  pre({ node: _node, className, ...props }) {
    return <pre {...props} className={cn('scrollbar-thin', className)} />
  },
  // SECURITY: cross-origin images from model output never auto-load. Shared
  // with every other renderer of untrusted Markdown — see the module for why.
  ...untrustedMarkdownImage,
  ...untrustedMarkdownLink,
}

/** Render one chunk of assistant prose as Markdown. */
function MarkdownText({ text }: { text: string }) {
  return (
    <div className={proseClasses}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkSoftBreaks]}
        // `detect` so unlabeled ``` fences (common in LLM output) still get
        // highlighted; `ignoreMissing` avoids throwing on an unknown language hint.
        rehypePlugins={[
          [rehypeHighlight, { detect: true, ignoreMissing: true }],
        ]}
        components={markdownComponents}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
}

/**
 * The body of an assistant turn: its ordered text/tool/permission segments, so
 * cards render inline where they occurred instead of all hoisted above the
 * prose. The activity indicator remains visible for the full server-owned
 * turn, including while tokens and tool cards stream into the message.
 */
function AssistantBody({
  message,
  streaming,
  activityLabel,
  turnStartedAt,
  pendingActionBasePath,
  conversationBasePath,
  conversationPublicId,
  onFix,
  onPermissionResolved,
}: {
  message: ChatMessage
  streaming: boolean
  activityLabel: string
  turnStartedAt?: string | null
  pendingActionBasePath: string
  conversationBasePath: string
  conversationPublicId: string | null
  onFix?: (text: string) => void
  onPermissionResolved?: () => void
}) {
  const parts = assistantParts(message)
  if (parts.length === 0) {
    return streaming ? (
      <TurnActivity label={activityLabel} startedAt={turnStartedAt} />
    ) : null
  }
  return (
    <>
      {parts.map((part, idx) =>
        part.type === 'tool' ? (
          // A `temps_write` tool is a *proposed* mutation — render the human
          // confirm/reject gate instead of a read-only result card.
          isTempsWriteToolName(part.tool.name) ? (
            <WriteProposalCard
              key={part.tool.id}
              pendingActionBasePath={pendingActionBasePath}
              tool={part.tool}
              onFix={onFix}
            />
          ) : (
            <ReadToolResultCard key={part.tool.id} tool={part.tool} />
          )
        ) : part.type === 'permission' ? (
          // ADR-038 Phase 2: interactive bridge permission request
          <PermissionCard
            key={`perm-${part.permission.id}`}
            conversationBasePath={conversationBasePath}
            conversationPublicId={conversationPublicId ?? ''}
            permission={part.permission}
            onResolved={onPermissionResolved}
          />
        ) : (
          <MarkdownText key={`text-${idx}`} text={part.text} />
        )
      )}
      {shouldShowAssistantActivityAfterContent(parts.length, streaming) && (
        <TurnActivity label={activityLabel} startedAt={turnStartedAt} />
      )}
    </>
  )
}

/** Streaming content does not imply completion; only the server terminal state does. */
export function shouldShowAssistantActivityAfterContent(
  partCount: number,
  streaming: boolean
) {
  return streaming && partCount > 0
}

/** Backoff schedule for WS reconnects — bounded, no thundering herd. */
const WS_RECONNECT_DELAYS_MS = [1000, 2000, 5000, 10000, 15000]

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

/**
 * Keeps a `GET .../conversations/{publicId}/stream` WebSocket open for the
 * panel's full lifetime. It is the single real-time transport for the sending
 * tab and every observer: tokens, tools, permissions, errors, and completion
 * all arrive here after the message command has been accepted.
 *
 * `suppressRef` is retained only for the permission-resolution history poll,
 * which deliberately replaces its own synthetic user-message echo with a
 * fresh authoritative snapshot.
 */
function useConversationStream(
  conversationBasePath: string,
  publicId: string | null,
  turnActiveRef: { current: boolean },
  suppressRef: { current: number },
  setMessages: SetMessages,
  setError: SetChatFailure,
  setWsTurnActive: React.Dispatch<React.SetStateAction<boolean>>,
  setTurnStartedAt: React.Dispatch<React.SetStateAction<string | null>>,
  setLiveUpdatesUnavailable: React.Dispatch<React.SetStateAction<boolean>>,
  onLiveEvent?: (eventName: string, data: string) => void
) {
  const onLiveEventRef = useRef(onLiveEvent)

  useEffect(() => {
    onLiveEventRef.current = onLiveEvent
  }, [onLiveEvent])

  useEffect(() => {
    setWsTurnActive(false)
    setTurnStartedAt(null)
    setLiveUpdatesUnavailable(false)
    if (!publicId) return
    let cancelled = false
    let ws: WebSocket | null = null
    let attempt = 0
    // True once this effect instance has completed at least one connection —
    // distinguishes the initial connect (history was already loaded by the
    // panel's own init fetch, no need to resync) from a later reconnect
    // (missed whatever happened while disconnected, including possibly a
    // `turn_complete` — resync and clear any stuck activity state rather
    // than trust stale local state).
    let hasConnectedBefore = false
    let liveEventRevision = 0
    let resyncRequestRevision = 0
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let fallbackPollTimer: ReturnType<typeof setTimeout> | null = null
    let activeTurnPollTimer: ReturnType<typeof setInterval> | null = null

    const resync = async () => {
      const requestedAtRevision = liveEventRevision
      const requestRevision = ++resyncRequestRevision
      try {
        const response = await fetch(`${conversationBasePath}/${publicId}`, {
          credentials: 'include',
        })
        const data = response.ok
          ? ((await response.json()) as ConversationDetailResponse)
          : null
        if (!cancelled && data) {
          const running = hasRunningServerTurn(
            data as typeof data & { turn_status?: string }
          )
          // A snapshot requested before a newer live event is stale by
          // definition. Never replace text, tools, or approvals that arrived
          // over the ordered WebSocket while this fetch was in flight.
          if (
            requestedAtRevision !== liveEventRevision ||
            requestRevision !== resyncRequestRevision
          ) {
            return running
          }
          setMessages((current) =>
            reconcileLatestHistoryPage(
              current,
              ensureRunningAssistant(mapConversationDetail(data), running)
            )
          )
          setWsTurnActive(running)
          setTurnStartedAt(running ? (data.turn_started_at ?? null) : null)
          return running
        }
      } catch {
        /* best-effort resync — the next event or poll retries */
      }
      return false
    }

    // The server owns turn and approval state. A WebSocket can appear open
    // while an intermediary silently drops an individual frame, so socket
    // health alone is not enough to prove the client is current. Reconcile a
    // running turn at low frequency even while the socket is connected. This
    // restores missed approvals and terminal state without polling idle chats;
    // revision checks above keep a late snapshot from overwriting newer wire
    // events.
    activeTurnPollTimer = setInterval(() => {
      if (turnActiveRef.current) void resync()
    }, 2000)

    const pollUntilTerminal = (): void => {
      if (cancelled) return
      void resync().then((running) => {
        if (!cancelled && running) {
          fallbackPollTimer = setTimeout(pollUntilTerminal, 2000)
        }
      })
    }

    const connect = () => {
      if (cancelled) return
      const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
      const socket = new WebSocket(
        `${wsProtocol}//${window.location.host}${conversationBasePath}/${publicId}/stream`
      )
      ws = socket
      socket.onopen = () => {
        attempt = 0
        if (fallbackPollTimer) {
          clearTimeout(fallbackPollTimer)
          fallbackPollTimer = null
        }
        setLiveUpdatesUnavailable(false)
        if (hasConnectedBefore) {
          onLiveEventRef.current?.('resync_required', '')
          void resync()
        }
        hasConnectedBefore = true
      }
      socket.onmessage = (ev) => {
        let frame: { event?: string; data?: string }
        try {
          frame = JSON.parse(ev.data as string) as {
            event?: string
            data?: string
          }
        } catch {
          return
        }
        const eventName = frame.event ?? ''
        const data = frame.data ?? ''
        liveEventRevision += 1
        onLiveEventRef.current?.(eventName, data)
        if (shouldSuppressPermissionPollEvent(eventName, suppressRef.current)) {
          return
        }
        if (eventName === 'resync_required') {
          setWsTurnActive(false)
          void resync()
          return
        }
        if (eventName === 'turn_complete') {
          setWsTurnActive(false)
          setTurnStartedAt(null)
          void resync()
          return
        }
        if (eventName === 'turn_state') {
          try {
            const state = JSON.parse(data) as {
              status?: string
              turn_started_at?: string | null
            }
            const running = turnStateNeedsResync(state.status)
            setWsTurnActive(running)
            setTurnStartedAt(running ? (state.turn_started_at ?? null) : null)
            // The initial WS snapshot contains lifecycle state, not the
            // pending permission payload. Reconcile immediately so a request
            // emitted before this tab subscribed is still actionable.
            if (running) void resync()
          } catch {
            setWsTurnActive(false)
            setTurnStartedAt(null)
          }
          return
        }
        if (eventName === 'runtime_options_updated') {
          try {
            const update = JSON.parse(data) as {
              auto_approved_permission_ids?: string[]
            }
            setMessages((messages) =>
              clearResolvedPermissionParts(
                messages,
                update.auto_approved_permission_ids ?? []
              )
            )
          } catch {
            /* a detail resync below remains authoritative */
          }
          void resync()
          return
        }
        if (eventName === 'error') {
          setWsTurnActive(false)
          setTurnStartedAt(null)
          applyWireEvent(eventName, data, setMessages, setError)
          return
        }
        if (eventName === 'user_message') {
          setWsTurnActive(true)
          try {
            const u = JSON.parse(data) as {
              content: string
              created_at?: string
              turn_id?: string
              attachments?: ChatAttachment[]
            }
            // This timestamp is produced by Temps when it persists the user
            // turn. It keeps already-connected observers on the same clock;
            // refresh/reconnect uses the exact persisted turn_started_at.
            setTurnStartedAt(u.created_at ?? null)
            setMessages((messages) => appendLiveUserTurn(messages, u))
          } catch {
            /* ignore malformed user_message frame */
          }
          return
        }
        applyWireEvent(eventName, data, setMessages, setError)
      }
      socket.onclose = () => {
        if (cancelled) return
        // Connectivity does not own the durable turn. Preserve the last
        // server snapshot so the composer cannot submit a conflicting turn.
        // A connection still being established is not an error and must not
        // hide the accepted turn; suppress activity only after retries are
        // exhausted and the stable unavailable state is visible.
        if (attempt >= WS_RECONNECT_DELAYS_MS.length) {
          // Surface one stable, non-blocking state only after bounded retries
          // instead of flashing an alert per reconnect.
          setLiveUpdatesUnavailable(true)
          pollUntilTerminal()
          return
        }
        const delay = WS_RECONNECT_DELAYS_MS[attempt]
        attempt += 1
        reconnectTimer = setTimeout(connect, delay)
      }
      socket.onerror = () => {
        // The close event owns retry and UI state. Handling both events caused
        // a disconnect/reconnect render flicker for each failed upgrade.
        socket.close()
      }
    }

    connect()
    return () => {
      cancelled = true
      if (reconnectTimer) clearTimeout(reconnectTimer)
      if (fallbackPollTimer) clearTimeout(fallbackPollTimer)
      if (activeTurnPollTimer) clearInterval(activeTurnPollTimer)
      ws?.close()
    }
  }, [
    conversationBasePath,
    publicId,
    turnActiveRef,
    suppressRef,
    setMessages,
    setError,
    setWsTurnActive,
    setTurnStartedAt,
    setLiveUpdatesUnavailable,
  ])
}

/**
 * The body of the AI debugging chat attached to any entity (ADR-023). Renders a
 * scrollable message list that fills its parent plus a follow-up composer — no
 * surrounding card, so it can drop into a sidebar/sheet or a page section. The
 * message submission is a short HTTP command and the conversation WebSocket
 * owns all real-time output; find/create/history use the generated SDK.
 */
export function DebugChatPanel({
  projectId,
  conversationPublicId,
  userScoped = false,
  contextType,
  contextId,
  startPrompt = 'Diagnose this and suggest concrete next steps.',
  autoStart = false,
  placeholder = 'Ask a follow-up…',
  lazyCreate = false,
  emptyHint = 'Ask anything about this project.',
  onConversationChange,
  onLiveEvent,
  onConversationStatusInvalidated,
}: DebugChatPanelProps) {
  const paths = chatApiPaths(userScoped, projectId)
  const base = paths.conversations
  const pendingActionBasePath = paths.pendingActions
  const ctxId = String(contextId)
  // Per-chat draft key: a half-typed message survives closing the dock,
  // switching chats, and reloads.
  const draftKey = `temps.ai.draft.${userScoped ? 'user' : projectId}:${contextType}:${ctxId}`
  // Current page context (what the user is viewing). Shown as a chip by the
  // input; the user can toggle whether it's attached.
  const { pageContext } = useAiAssistant()
  const [includeContext, setIncludeContext] = useState(true)
  const [publicId, setPublicId] = useState<string | null>(
    conversationPublicId ?? null
  )
  const providerPinnedRef = useRef(false)
  const providerStatusRequestRef = useRef(0)
  const [providerOptions, setProviderOptions] = useState<ChatProviderOption[]>(
    []
  )
  const [providerStatusState, setProviderStatusState] = useState<
    'loading' | 'success' | 'error'
  >('loading')
  const [providerRefreshing, setProviderRefreshing] = useState(false)
  const [runtimeSelection, setRuntimeSelection] =
    useState<ChatRuntimeSelection>({
      providerId: 'gateway',
      modelId: null,
      thinkingOptionId: null,
      permissionModeId: null,
    })
  const selectedProvider = runtimeSelection.providerId
  const selectedProviderOption = providerOptions.find(
    (provider) => provider.id === selectedProvider
  )
  const selectedModelOption = selectedProviderOption?.models.find(
    (model) => model.id === runtimeSelection.modelId
  )
  // Some providers (OpenRouter especially) advertise hundreds of models —
  // a plain unsearchable dropdown becomes unusable at that size.
  const modelOptions: SearchableSelectOption[] = (() => {
    const provider = selectedProviderOption
    if (!provider) return []
    return provider.models.map((model) => ({
      value: model.id,
      label: chatModelLabel(provider, model),
      keywords: model.id,
    }))
  })()
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [pendingAttachments, setPendingAttachments] = useState<
    ChatAttachment[]
  >([])
  const [attachmentUploads, setAttachmentUploads] = useState(0)
  const attachmentInputRef = useRef<HTMLInputElement>(null)
  const [input, setInput] = useState(() => {
    try {
      return localStorage.getItem(draftKey) ?? ''
    } catch {
      return ''
    }
  })
  const [streaming, setStreaming] = useState(false)
  const [starting, setStarting] = useState(false)
  // True until the run-once init fetch resolves. Lets us show a skeleton instead
  // of flashing the "Start AI diagnosis" empty state while resuming a chat —
  // that empty condition is indistinguishable from the initial mount state.
  const [initializing, setInitializing] = useState(true)
  const [historyLoadError, setHistoryLoadError] = useState<string | null>(null)
  const [historyPage, setHistoryPage] = useState<ConversationHistoryPage>({
    has_more: false,
    next_before: null,
  })
  const [loadingEarlierMessages, setLoadingEarlierMessages] = useState(false)
  const [earlierMessagesError, setEarlierMessagesError] = useState<
    string | null
  >(null)
  const [historyReloadNonce, setHistoryReloadNonce] = useState(0)
  const retryConversationHistory = useCallback(() => {
    setInitializing(true)
    setHistoryLoadError(null)
    setHistoryReloadNonce((nonce) => nonce + 1)
  }, [])
  const [error, setError] = useState<ChatFailure | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const historyPageRequestRef = useRef(false)
  const prependScrollAnchorRef = useRef<{
    scrollHeight: number
    scrollTop: number
  } | null>(null)
  const followTranscriptRef = useRef(true)
  const [showJumpToLatest, setShowJumpToLatest] = useState(false)
  const queuedInterruptRef = useRef<string | null>(null)
  const sendAfterInterruptRef = useRef<(text: string) => void>(() => {})
  const submissionInFlightRef = useRef(false)
  // Counts history-replacing permission resolution polls. Ordinary messages
  // never suppress the WebSocket because it is their only live transport.
  const wsSuppressRef = useRef(0)
  // Authoritative running state from the durable snapshot/live wire. This is
  // not limited to another tab: the sending tab receives the same state.
  const [wsTurnActive, setWsTurnActive] = useState(false)
  const [turnStartedAt, setTurnStartedAt] = useState<string | null>(null)
  const [liveUpdatesUnavailable, setLiveUpdatesUnavailable] = useState(false)
  const turnActive = streaming || wsTurnActive
  const turnActiveRef = useRef(turnActive)
  useEffect(() => {
    turnActiveRef.current = turnActive
  }, [turnActive])

  const changePermissionMode = useCallback(
    async (permissionModeId: string) => {
      const previousPermissionModeId = runtimeSelection.permissionModeId
      setRuntimeSelection((selection) => ({
        ...selection,
        permissionModeId,
      }))
      if (!turnActive || !publicId) return

      try {
        const response = await fetch(`${base}/${publicId}/permission-mode`, {
          method: 'POST',
          credentials: 'include',
          headers: {
            'Content-Type': 'application/json',
            Accept: 'application/json',
          },
          body: JSON.stringify({ permission_mode: permissionModeId }),
        })
        const payload = (await response.json().catch(() => ({}))) as {
          ai_permission_mode?: string
          title?: string
          detail?: string
        }
        if (!response.ok) {
          setRuntimeSelection((selection) => ({
            ...selection,
            permissionModeId: previousPermissionModeId,
          }))
          setError(chatFailureFromProblem(payload, response.status))
          return
        }
        setRuntimeSelection((selection) => ({
          ...selection,
          permissionModeId: payload.ai_permission_mode ?? permissionModeId,
        }))
      } catch {
        setRuntimeSelection((selection) => ({
          ...selection,
          permissionModeId: previousPermissionModeId,
        }))
        setError(
          localChatFailure(
            'Could not change permissions',
            'Temps could not update the active turn. The existing approval mode is still in effect.',
            'permission_mode_update_failed'
          )
        )
      }
    },
    [base, publicId, runtimeSelection.permissionModeId, turnActive]
  )

  const loadProviderStatus = useCallback(
    async (forceRefresh = false, silent = false) => {
      const requestGeneration = ++providerStatusRequestRef.current
      if (forceRefresh) setProviderRefreshing(true)
      else setProviderStatusState('loading')
      try {
        const options = usesHarnessCatalog(contextType)
          ? chatHarnessProviderOptions(
              (
                await listAiProviders({
                  query: {
                    catalog_only: false,
                    refresh_models: forceRefresh,
                  },
                  throwOnError: true,
                })
              ).data.providers
            )
          : (
              (forceRefresh
                ? await refreshAiProviderStatus({ throwOnError: true })
                : await getAiProviderStatus({ throwOnError: true })
              ).data?.available_providers ?? []
            ).map((provider) => {
              const extended = provider as typeof provider & ChatProviderOption
              return {
                id: provider.id,
                name: provider.name,
                auth_source: provider.auth_source,
                models: extended.models ?? [],
                default_model_id: extended.default_model_id,
                model_discovery_status: extended.model_discovery_status,
                model_discovery_error: extended.model_discovery_error,
                permission_modes: extended.permission_modes ?? [],
                default_permission_mode_id: extended.default_permission_mode_id,
              }
            })
        if (requestGeneration !== providerStatusRequestRef.current) return
        setProviderOptions((current) =>
          providerPinnedRef.current
            ? [
                ...options,
                ...current.filter(
                  (saved) => !options.some((option) => option.id === saved.id)
                ),
              ]
            : options
        )

        if (providerPinnedRef.current) {
          setRuntimeSelection((current) =>
            reconcileChatRuntimeAfterRefresh(options, current)
          )
        }

        const active = 'gateway'
        if (
          !providerPinnedRef.current &&
          active &&
          options.some((option) => option.id === active)
        ) {
          setRuntimeSelection(resolveChatRuntimeSelection(options, active))
        } else if (!providerPinnedRef.current && options[0]) {
          setRuntimeSelection(
            resolveChatRuntimeSelection(options, options[0].id)
          )
        }
        setProviderStatusState('success')
      } catch {
        if (requestGeneration !== providerStatusRequestRef.current) return
        if (forceRefresh && !silent) {
          toast.error('Couldn’t refresh provider authentication and models')
        } else if (!forceRefresh) {
          setProviderStatusState('error')
        }
      } finally {
        if (requestGeneration === providerStatusRequestRef.current) {
          setProviderRefreshing(false)
        }
      }
    },
    [contextType]
  )

  useEffect(() => {
    // First paint uses cached/bootstrap capabilities and never invokes a CLI.
    // Account-aware names/auth are refreshed silently after the composer is
    // usable; the explicit refresh button uses the same path with error UI.
    const timer = window.setTimeout(() => void loadProviderStatus(), 0)
    const refreshTimer = usesHarnessCatalog(contextType)
      ? window.setTimeout(() => void loadProviderStatus(true, true), 250)
      : null
    return () => {
      window.clearTimeout(timer)
      if (refreshTimer != null) window.clearTimeout(refreshTimer)
    }
  }, [contextType, loadProviderStatus])

  useConversationStream(
    base,
    publicId,
    turnActiveRef,
    wsSuppressRef,
    setMessages,
    setError,
    setWsTurnActive,
    setTurnStartedAt,
    setLiveUpdatesUnavailable,
    onLiveEvent
  )

  const stop = useCallback(() => {
    if (!publicId) return
    // Execution is server-owned. Stop is therefore an explicit mutation; a
    // client disconnect or cancelled HTTP request never owns task lifetime.
    void fetch(`${base}/${publicId}/stop`, {
      method: 'POST',
      credentials: 'include',
    })
      .then((response) => {
        if (!response.ok) {
          throw new Error(`Could not stop turn (${response.status})`)
        }
        setWsTurnActive(false)
        onConversationStatusInvalidated?.()
        const queuedInterrupt = queuedInterruptRef.current
        queuedInterruptRef.current = null
        if (queuedInterrupt) {
          window.setTimeout(
            () => sendAfterInterruptRef.current(queuedInterrupt),
            0
          )
        }
      })
      .catch((stopError) => {
        setError(
          localChatFailure(
            'Could not stop the running turn',
            stopError instanceof Error
              ? stopError.message
              : 'Temps could not stop the active turn. Check the connection and try again.',
            'turn_stop_failed'
          )
        )
      })
  }, [base, onConversationStatusInvalidated, publicId])

  const composerRef = useRef<HTMLTextAreaElement>(null)
  const composerDisabled = isChatComposerDisabled(
    Boolean(publicId),
    starting,
    lazyCreate
  )

  // `autoFocus` handles a composer that mounts enabled. Existing chats first
  // mount disabled while their conversation id loads, so focus again whenever
  // the composer becomes usable. This also returns keyboard focus after a turn
  // finishes, matching normal chat-composer behaviour.
  useEffect(() => {
    if (composerDisabled) return
    const frame = window.requestAnimationFrame(() => {
      composerRef.current?.focus({ preventScroll: true })
    })
    return () => window.cancelAnimationFrame(frame)
  }, [composerDisabled, projectId, contextType, ctxId])

  // Keep short prompts compact, grow with wrapped/new lines, then hand longer
  // drafts to an internal scrollbar so the composer cannot consume the chat.
  useLayoutEffect(() => {
    const textarea = composerRef.current
    if (!textarea) return

    const resize = () => {
      textarea.style.height = '0px'
      const layout = resolveChatComposerLayout(textarea.scrollHeight)
      textarea.style.height = `${layout.height}px`
      textarea.style.overflowY = layout.overflowY
    }
    resize()

    // The AI dock panel animates its width in on open (`transition-[width]`
    // in AiAssistantDock), and this composer mounts immediately rather than
    // after that transition finishes. So this effect's first run can measure
    // scrollHeight while the panel is still narrow mid-animation, wrapping
    // the placeholder onto extra lines and inflating the clamp to its max —
    // stuck there until `input` next changes. Re-run once the composer's
    // actual width settles instead of only reacting to typed input.
    const observer = new ResizeObserver(resize)
    observer.observe(textarea)
    return () => observer.disconnect()
  }, [input])

  // Persist the draft as the user types; clear it once sent (input → '').
  useEffect(() => {
    try {
      if (input) localStorage.setItem(draftKey, input)
      else localStorage.removeItem(draftKey)
    } catch {
      /* storage unavailable — non-fatal */
    }
  }, [input, draftKey])

  const send = useCallback(
    async (text: string, conversationId?: string) => {
      let id = conversationId ?? publicId
      const content = text.trim()
      const attachments = pendingAttachments
      // Need either an existing conversation or permission to create one lazily.
      if (
        (!content && attachments.length === 0) ||
        attachmentUploads > 0 ||
        (!id && !lazyCreate) ||
        submissionInFlightRef.current
      )
        return
      submissionInFlightRef.current = true
      let submissionStarted = false
      const turnId = crypto.randomUUID()
      try {
        // Refresh the server-owned lifecycle and message history before making
        // any optimistic change. This closes the refresh/double-send gap where
        // a remounted tab believed the thread was idle while the server still
        // owned a running turn.
        if (id) {
          const snapshotResponse = await fetch(`${base}/${id}`, {
            credentials: 'include',
            cache: 'no-store',
          })
          const snapshotPayload = (await snapshotResponse
            .json()
            .catch(() => ({}))) as ConversationDetailResponse & {
            detail?: string
          }
          if (!snapshotResponse.ok) {
            setError(
              chatFailureFromProblem(snapshotPayload, snapshotResponse.status)
            )
            return
          }
          const running = hasRunningServerTurn(snapshotPayload)
          setMessages((current) =>
            reconcileLatestHistoryPage(
              current,
              ensureRunningAssistant(
                mapConversationDetail(snapshotPayload),
                running
              )
            )
          )
          setWsTurnActive(running)
          setTurnStartedAt(
            running ? (snapshotPayload.turn_started_at ?? null) : null
          )
          if (running) {
            setError(
              localChatFailure(
                'A turn is already running',
                'Temps refreshed this thread and found an active server-owned turn. Stop it or wait for it to finish before sending another message.',
                'turn_in_progress'
              )
            )
            return
          }
        }

        setError(null)
        setTurnStartedAt(null)
        setStreaming(true)
        submissionStarted = true
        // The preflight above established an idle durable snapshot. Render the
        // accepted command optimistically, then reconcile its WebSocket echo by
        // turn id; the atomic server claim remains the final concurrency gate.
        const now = new Date().toISOString()
        setMessages((m) => [
          ...m,
          {
            role: 'user',
            content,
            attachments,
            created_at: now,
            client_turn_id: turnId,
          },
          {
            role: 'assistant',
            content: '',
            created_at: now,
            client_turn_id: turnId,
          },
        ])
        // Lazy-create the conversation on the first message (new project chat).
        if (!id) {
          if (userScoped || projectId == null) {
            setError(
              localChatFailure(
                'Thread no longer exists',
                'This user-owned thread could not be found. Create a new thread and send the message again.',
                'conversation_not_found',
                false
              )
            )
            dropOptimisticTurn(setMessages, turnId)
            return
          }
          const { data: conv, error: problem } = await createConversation({
            path: { project_id: projectId },
            body: {
              context_type: contextType,
              context_id: ctxId,
              ai_provider: selectedProvider,
              ai_model: runtimeSelection.modelId,
              ai_thinking_level: runtimeSelection.thinkingOptionId,
              ai_permission_mode: runtimeSelection.permissionModeId,
            },
          })
          if (!conv) {
            setError(chatFailureFromProblem(problem))
            dropOptimisticTurn(setMessages, turnId)
            return
          }
          id = conv.public_id
          providerPinnedRef.current = true
          setPublicId(conv.public_id)
        }
        const res = await fetch(`${base}/${id}/messages`, {
          method: 'POST',
          credentials: 'include',
          headers: {
            'Content-Type': 'application/json',
            Accept: 'application/json',
          },
          body: JSON.stringify({
            content,
            attachments: attachments.map(({ id, name }) => ({ id, name })),
            turn_id: turnId,
            ai_model: runtimeSelection.modelId,
            ai_thinking_level: runtimeSelection.thinkingOptionId,
            ai_permission_mode: runtimeSelection.permissionModeId,
            // Ephemeral framing about the page the user is on — not stored or
            // shown; the backend attaches it to this turn only. Honours the
            // user's include toggle.
            page_context:
              includeContext && pageContext ? pageContext.value : undefined,
          }),
        })
        const responsePayload = (await res.json().catch(() => ({}))) as {
          detail?: string
          turn_started_at?: string
        }
        onConversationStatusInvalidated?.()
        if (!res.ok) {
          const problem = responsePayload
          if (res.status === 409) {
            // Another tab may have won the atomic claim after our preflight.
            // Immediately replace optimism with the authoritative snapshot.
            const latest = await fetch(`${base}/${id}`, {
              credentials: 'include',
              cache: 'no-store',
            })
              .then(async (response) =>
                response.ok
                  ? ((await response.json()) as ConversationDetailResponse)
                  : null
              )
              .catch(() => null)
            if (latest) {
              const running = hasRunningServerTurn(latest)
              setMessages((current) =>
                reconcileLatestHistoryPage(
                  current,
                  ensureRunningAssistant(mapConversationDetail(latest), running)
                )
              )
              setWsTurnActive(running)
              setTurnStartedAt(
                running ? (latest.turn_started_at ?? null) : null
              )
            } else {
              setWsTurnActive(true)
              dropOptimisticTurn(setMessages, turnId)
            }
            setError(chatFailureFromProblem(problem, res.status))
          } else {
            setError(chatFailureFromProblem(problem, res.status))
            dropOptimisticTurn(setMessages, turnId)
          }
          return
        }
        setInput('')
        setPendingAttachments([])
        if (typeof responsePayload.turn_started_at === 'string') {
          setTurnStartedAt(responsePayload.turn_started_at)
        }
        setWsTurnActive(true)
      } catch {
        setError(
          localChatFailure(
            'Could not reach Temps',
            submissionStarted
              ? 'The message could not be submitted because the browser lost its connection to Temps. Reconnect and try again.'
              : 'Temps could not refresh the server-owned thread state, so the message was not sent. Reconnect and try again.',
            'chat_connection_failed'
          )
        )
        if (submissionStarted) dropOptimisticTurn(setMessages, turnId)
      } finally {
        if (submissionStarted) setStreaming(false)
        submissionInFlightRef.current = false
      }
    },
    [
      base,
      publicId,
      lazyCreate,
      userScoped,
      projectId,
      contextType,
      ctxId,
      pageContext,
      includeContext,
      selectedProvider,
      runtimeSelection.modelId,
      runtimeSelection.thinkingOptionId,
      runtimeSelection.permissionModeId,
      pendingAttachments,
      attachmentUploads,
      onConversationStatusInvalidated,
    ]
  )

  useEffect(() => {
    sendAfterInterruptRef.current = (text) => void send(text)
  }, [send])
  const submitComposer = useCallback(() => {
    if (!input.trim() && pendingAttachments.length > 0 && !turnActive) {
      void send('')
      return
    }
    const action = chatComposerSubmitAction(input, turnActive)
    if (action === 'none') return
    if (action === 'interrupt-and-send') {
      queuedInterruptRef.current = input.trim()
      setInput('')
      stop()
      return
    }
    void send(input)
  }, [input, pendingAttachments.length, turnActive, send, stop])

  const uploadAttachments = useCallback(
    async (files: FileList | null) => {
      if (!files || !publicId || !userScoped) return
      const available = Math.max(0, 8 - pendingAttachments.length)
      const selected = Array.from(files).slice(0, available)
      if (selected.length === 0) {
        setError(
          localChatFailure(
            'Attachment limit reached',
            'A message may include at most 8 files.',
            'attachment_limit'
          )
        )
        return
      }
      setAttachmentUploads((count) => count + selected.length)
      setError(null)
      await Promise.all(
        selected.map(async (file) => {
          if (file.size > 20 * 1024 * 1024) {
            setError(
              localChatFailure(
                'File is too large',
                `${file.name} exceeds the 20 MB attachment limit.`,
                'attachment_too_large'
              )
            )
            return
          }
          const form = new FormData()
          form.append('file', file)
          try {
            const response = await fetch(`${base}/${publicId}/attachments`, {
              method: 'POST',
              credentials: 'include',
              body: form,
            })
            const payload = (await response.json().catch(() => ({}))) as
              (ChatAttachment & { detail?: string }) | { detail?: string }
            if (!response.ok || !('id' in payload)) {
              throw new Error(
                payload.detail ?? `Could not upload ${file.name}.`
              )
            }
            const attachment: ChatAttachment = {
              ...payload,
              preview_url: payload.is_image
                ? URL.createObjectURL(file)
                : undefined,
            }
            setPendingAttachments((current) => [...current, attachment])
          } catch (cause) {
            setError(
              localChatFailure(
                'Could not attach file',
                cause instanceof Error
                  ? cause.message
                  : `Could not upload ${file.name}.`,
                'attachment_upload_failed'
              )
            )
          }
        })
      )
      setAttachmentUploads((count) => Math.max(0, count - selected.length))
      if (attachmentInputRef.current) attachmentInputRef.current.value = ''
    },
    [base, pendingAttachments.length, publicId, userScoped]
  )

  const removePendingAttachment = useCallback((id: string) => {
    setPendingAttachments((current) => {
      const removed = current.find((attachment) => attachment.id === id)
      if (removed?.preview_url) URL.revokeObjectURL(removed.preview_url)
      return current.filter((attachment) => attachment.id !== id)
    })
  }, [])

  const start = useCallback(async () => {
    if (userScoped || projectId == null) {
      setError(
        localChatFailure(
          'Thread must be created from an application',
          'Create this user-owned thread from the application workspace, then send the message again.',
          'invalid_thread_scope',
          false
        )
      )
      return
    }
    setStarting(true)
    setError(null)
    try {
      const { data: conv, error: problem } = await createConversation({
        path: { project_id: projectId },
        body: {
          context_type: contextType,
          context_id: ctxId,
          ai_provider: selectedProvider,
          ai_model: runtimeSelection.modelId,
          ai_thinking_level: runtimeSelection.thinkingOptionId,
          ai_permission_mode: runtimeSelection.permissionModeId,
        },
      })
      if (!conv) {
        setError(chatFailureFromProblem(problem))
        return
      }
      const extended = conv as typeof conv & {
        ai_model?: string | null
        ai_thinking_level?: string | null
        ai_permission_mode?: string | null
      }
      setRuntimeSelection(
        resolveChatRuntimeSelection(providerOptions, conv.ai_provider, {
          modelId: extended.ai_model,
          thinkingOptionId: extended.ai_thinking_level,
          permissionModeId: extended.ai_permission_mode,
        })
      )
      setPublicId(conv.public_id)
      providerPinnedRef.current = true
      setMessages([])
      setHistoryPage({ has_more: false, next_before: null })
      setEarlierMessagesError(null)
      void send(startPrompt, conv.public_id)
    } catch {
      setError(
        localChatFailure(
          'Could not start the chat',
          'Temps could not create the conversation. Check the connection and configured AI harness, then retry.',
          'conversation_create_failed'
        )
      )
    } finally {
      setStarting(false)
    }
  }, [
    projectId,
    userScoped,
    contextType,
    ctxId,
    startPrompt,
    send,
    selectedProvider,
    runtimeSelection.modelId,
    runtimeSelection.thinkingOptionId,
    runtimeSelection.permissionModeId,
    providerOptions,
  ])

  // Keep the latest send/start in refs so the one-shot init effect below can
  // call them without listing them as dependencies (which would make it re-run
  // every time `publicId` changes — reloading history on top of the live stream
  // and duplicating turns).
  const startRef = useRef(start)
  useEffect(() => {
    startRef.current = start
  }, [start])

  // Initialise exactly once per mount: load the existing conversation for this
  // context, or auto-start a fresh one. The panel is re-keyed per context by its
  // parent, so a context switch is a remount — hence run-once is correct.
  useEffect(() => {
    let ignore = false
    ;(async () => {
      try {
        let initialDetail: ConversationDetailResponse | null = null
        let conv: ConversationResponse | null = null
        if (userScoped) {
          if (!conversationPublicId) return
          const response = await fetch(`${base}/${conversationPublicId}`, {
            credentials: 'include',
          })
          if (!response.ok) {
            throw new Error(conversationHistoryErrorMessage(response.status))
          }
          initialDetail = (await response.json()) as PaginatedConversationDetail
          conv = initialDetail
        } else if (projectId != null) {
          const result = await findConversation({
            path: { project_id: projectId },
            query: { context_type: contextType, context_id: ctxId },
          })
          conv = result.data ?? null
        }
        if (ignore) return
        if (!conv) {
          if (autoStart) void startRef.current()
          return
        }
        providerPinnedRef.current = true
        const extended = conv as typeof conv & {
          ai_model?: string | null
          ai_thinking_level?: string | null
          ai_permission_mode?: string | null
        }
        setProviderOptions((options) => {
          const next = options.some((option) => option.id === conv.ai_provider)
            ? options
            : [
                ...options,
                {
                  id: conv.ai_provider,
                  name: providerDisplayName(conv.ai_provider),
                  auth_source: conv.ai_provider.startsWith('gateway')
                    ? 'configured_key'
                    : 'host_environment',
                  models: extended.ai_model
                    ? [
                        {
                          id: extended.ai_model,
                          name: extended.ai_model,
                          thinking_options: extended.ai_thinking_level
                            ? [
                                {
                                  id: extended.ai_thinking_level,
                                  name: extended.ai_thinking_level,
                                },
                              ]
                            : [],
                          default_thinking_option_id:
                            extended.ai_thinking_level,
                        },
                      ]
                    : [],
                  default_model_id: extended.ai_model,
                  permission_modes: extended.ai_permission_mode
                    ? [
                        {
                          id: extended.ai_permission_mode,
                          name: extended.ai_permission_mode,
                        },
                      ]
                    : [],
                  default_permission_mode_id: extended.ai_permission_mode,
                },
              ]
          setRuntimeSelection(
            resolveChatRuntimeSelection(next, conv.ai_provider, {
              modelId: extended.ai_model,
              thinkingOptionId: extended.ai_thinking_level,
              permissionModeId: extended.ai_permission_mode,
            })
          )
          return next
        })
        setPublicId(conv.public_id)
        const detail =
          initialDetail ??
          (projectId != null
            ? (
                await getConversation({
                  path: { project_id: projectId, public_id: conv.public_id },
                  throwOnError: true,
                })
              ).data
            : null)
        if (!ignore && detail) {
          const running = hasRunningServerTurn(
            detail as typeof detail & { turn_status?: string }
          )
          setMessages(
            ensureRunningAssistant(mapConversationDetail(detail), running)
          )
          setHistoryPage(
            conversationHistoryPage(detail as PaginatedConversationDetail)
          )
          setWsTurnActive(running)
          setTurnStartedAt(running ? (detail.turn_started_at ?? null) : null)
        }
      } catch (cause) {
        if (!ignore) {
          setHistoryLoadError(
            cause instanceof Error &&
              cause.message.startsWith('Couldn’t load this conversation')
              ? cause.message
              : conversationHistoryErrorMessage()
          )
        }
      } finally {
        if (!ignore) setInitializing(false)
      }
    })()
    return () => {
      ignore = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [historyReloadNonce])

  const loadEarlierMessages = useCallback(async () => {
    const before = historyPage.next_before
    if (
      !publicId ||
      !historyPage.has_more ||
      !before ||
      historyPageRequestRef.current
    ) {
      return
    }

    historyPageRequestRef.current = true
    setLoadingEarlierMessages(true)
    setEarlierMessagesError(null)
    const viewport = scrollRef.current
    prependScrollAnchorRef.current = viewport
      ? {
          scrollHeight: viewport.scrollHeight,
          scrollTop: viewport.scrollTop,
        }
      : null

    try {
      const response = await fetch(
        `${base}/${publicId}?before=${encodeURIComponent(before)}&limit=50`,
        {
          credentials: 'include',
          cache: 'no-store',
        }
      )
      const detail = (await response
        .json()
        .catch(() => ({}))) as PaginatedConversationDetail & {
        detail?: string
      }
      if (!response.ok) {
        throw new Error(
          detail.detail ??
            `Couldn’t load earlier messages (HTTP ${response.status}).`
        )
      }

      const olderMessages = mapConversationDetail({
        ...detail,
        pending_permission: null,
      })
      setMessages((current) => prependHistoryPage(current, olderMessages))
      setHistoryPage(conversationHistoryPage(detail))
    } catch (cause) {
      prependScrollAnchorRef.current = null
      setEarlierMessagesError(
        cause instanceof Error
          ? cause.message
          : 'Couldn’t load earlier messages. Scroll up to retry.'
      )
    } finally {
      historyPageRequestRef.current = false
      setLoadingEarlierMessages(false)
    }
  }, [base, historyPage, publicId])

  const handleTranscriptScroll = useCallback(() => {
    const viewport = scrollRef.current
    if (!viewport) return
    if (shouldLoadEarlierMessages(viewport.scrollTop, historyPage.has_more)) {
      followTranscriptRef.current = false
      setShowJumpToLatest(true)
      void loadEarlierMessages()
      return
    }
    const nearBottom = isChatTranscriptNearBottom(viewport)
    followTranscriptRef.current = nearBottom
    setShowJumpToLatest(!nearBottom)
  }, [historyPage.has_more, loadEarlierMessages])

  const scrollTranscriptToBottom = useCallback(() => {
    const viewport = scrollRef.current
    if (!viewport) return
    followTranscriptRef.current = true
    setShowJumpToLatest(false)
    viewport.scrollTo({ top: viewport.scrollHeight, behavior: 'smooth' })
  }, [])

  useLayoutEffect(() => {
    const anchor = prependScrollAnchorRef.current
    if (!anchor) return
    prependScrollAnchorRef.current = null
    const viewport = scrollRef.current
    if (!viewport) return
    viewport.scrollTop = restoredHistoryScrollTop(anchor, viewport.scrollHeight)
  }, [messages])

  useLayoutEffect(() => {
    if (!followTranscriptRef.current) return
    const viewport = scrollRef.current
    if (!viewport) return
    viewport.scrollTo({ top: viewport.scrollHeight })
  }, [messages])

  // Fallback for when the WebSocket connection that was open when a question was
  // asked has died by the time the user answers it (backgrounded tab, network
  // blip, server restart). `resolvePermission` still delivers the answer to
  // the model and the reply still gets persisted — this just polls history
  // for it so it appears without a manual reload. Stops as soon as the server
  // has more messages than we've rendered, or after a bounded number of tries.
  const pollForReply = useCallback(() => {
    if (!publicId) return
    let cancelled = false
    let attempt = 0
    // The permission-resolve request also triggers a `user_message` broadcast
    // (the synthetic "you answered" turn) — suppress the WS echo of that on
    // this tab for the duration of the poll, since this refetch already
    // covers it.
    wsSuppressRef.current += 1
    let released = false
    const release = () => {
      if (released) return
      released = true
      wsSuppressRef.current = Math.max(0, wsSuppressRef.current - 1)
    }
    const poll = async () => {
      if (cancelled || attempt >= 6) {
        release()
        return
      }
      attempt += 1
      const detail = await fetch(`${base}/${publicId}`, {
        credentials: 'include',
      })
        .then(async (response) =>
          response.ok
            ? ((await response.json()) as ConversationDetailResponse)
            : null
        )
        .catch(() => null)
      if (cancelled || !detail) {
        release()
        return
      }
      const hasPendingPermission = Boolean(detail.pending_permission)
      const running = hasRunningServerTurn(detail)
      const terminal = permissionPollIsTerminal(
        detail.turn_status,
        hasPendingPermission
      )
      if (terminal) {
        setMessages((current) =>
          reconcileLatestHistoryPage(current, mapConversationDetail(detail))
        )
        setWsTurnActive(false)
        setTurnStartedAt(null)
        release()
        return
      }
      setMessages((current) =>
        reconcileLatestHistoryPage(
          current,
          ensureRunningAssistant(mapConversationDetail(detail), running)
        )
      )
      setWsTurnActive(running)
      setTurnStartedAt(running ? (detail.turn_started_at ?? null) : null)
      // A non-terminal snapshot is only an intermediate recovery point. Keep
      // polling until the server owns a terminal state; otherwise an optimistic
      // assistant placeholder can make equal message counts look "caught up".
      setTimeout(poll, hasPendingPermission ? 2000 : 500)
    }
    void poll()
    return () => {
      cancelled = true
      release()
    }
  }, [base, publicId])

  // Report the active conversation id upward (lets the dock reset it).
  useEffect(() => {
    onConversationChange?.(publicId)
  }, [publicId, onConversationChange])

  const visible = messages.filter((m) => m.role !== 'system')
  const busy = turnActive || starting
  // A turn in flight either from this tab's own send() or observed live from
  // another tab over the WS — both need the activity indicator to show.
  const liveTurn = shouldShowLiveTurn(
    streaming,
    wsTurnActive,
    liveUpdatesUnavailable
  )
  // Show a standalone activity row only before the optimistic assistant turn
  // exists (i.e. while the conversation is being created).
  const showBootRow = visible.length === 0 && busy
  const showTrailingActivityRow =
    !showBootRow &&
    needsTrailingActivityRow(liveTurn, visible[visible.length - 1]?.role)
  const activityLabel = chatTurnActivityLabel(contextType, streaming)

  return (
    <div className="flex h-full min-h-0 flex-col gap-3">
      <div className="relative min-h-0 flex-1">
        <div
          ref={scrollRef}
          onScroll={handleTranscriptScroll}
          className="h-full space-y-4 overflow-y-auto pr-1"
        >
          {loadingEarlierMessages && (
            <div
              className="flex items-center justify-center gap-2 py-2 text-xs text-muted-foreground"
              role="status"
            >
              <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
              Loading earlier messages…
            </div>
          )}

          {earlierMessagesError && historyPage.has_more && (
            <div
              className="flex items-center justify-center gap-2 py-2 text-xs text-destructive"
              role="alert"
            >
              <span>{earlierMessagesError}</span>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="h-7 px-2"
                onClick={() => void loadEarlierMessages()}
              >
                Retry
              </Button>
            </div>
          )}

          {/* Until the run-once init fetch resolves we can't tell "no chat yet"
            (show the start button) apart from "resuming an existing chat"
            (about to load history) — both look like the empty mount state. Show
            a skeleton meanwhile so resuming doesn't flash "Start AI diagnosis". */}
          {initializing && visible.length === 0 && !busy && (
            <div className="space-y-4">
              <div className="flex items-start gap-2.5">
                <Skeleton className="h-7 w-7 shrink-0 rounded-full" />
                <Skeleton className="h-16 flex-1 rounded-2xl rounded-tl-sm" />
              </div>
              <div className="flex justify-end">
                <Skeleton className="h-9 w-2/3 rounded-2xl rounded-tr-sm" />
              </div>
            </div>
          )}

          {!initializing && historyLoadError && visible.length === 0 && (
            <div
              className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center"
              role="alert"
            >
              <Info className="h-6 w-6 text-destructive" />
              <p className="max-w-md text-sm text-muted-foreground">
                {historyLoadError}
              </p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={retryConversationHistory}
              >
                <RefreshCw className="mr-2 h-3.5 w-3.5" />
                Retry conversation
              </Button>
            </div>
          )}

          {!initializing &&
            !historyLoadError &&
            visible.length === 0 &&
            !busy &&
            !publicId &&
            (lazyCreate ? (
              // Free-form chat (e.g. a project chat): nothing to auto-diagnose, so
              // invite the user to type — the first message creates the chat.
              <div className="flex h-full flex-col items-center justify-center gap-2 text-center">
                <Sparkles className="h-6 w-6 text-muted-foreground" />
                <p className="max-w-xs text-sm text-muted-foreground">
                  {emptyHint}
                </p>
              </div>
            ) : (
              <div className="flex h-full flex-col items-center justify-center gap-3 text-center">
                <Sparkles className="h-6 w-6 text-muted-foreground" />
                <Button
                  onClick={() => void start()}
                  disabled={starting || providerOptions.length === 0}
                >
                  {starting ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <Sparkles className="h-4 w-4" />
                  )}
                  <span className="ml-2">Start AI diagnosis</span>
                </Button>
              </div>
            ))}

          {showBootRow && (
            <div className="flex items-start">
              <div className="flex items-center gap-2 rounded-2xl rounded-tl-sm bg-muted/60 px-3.5 py-2.5 text-sm text-muted-foreground">
                <TurnActivity label={activityLabel} startedAt={turnStartedAt} />
                Reading logs and analyzing the failure…
              </div>
            </div>
          )}

          {visible.map((m, i) => {
            const isTrailing = i === visible.length - 1
            // A completed assistant turn with nothing extracted (e.g. the CLI's
            // only action was a tool call CLI-chat can't bridge — ADR-038)
            // renders as truly nothing, not an empty styled bubble: the padded,
            // rounded `bg-muted` wrapper below has no way to look "empty" once
            // it exists, so skip the whole message instead of leaving a blank
            // gray bar in the transcript that reads as an answer with no text.
            if (
              m.role === 'assistant' &&
              assistantParts(m).length === 0 &&
              !(liveTurn && isTrailing)
            ) {
              return null
            }
            return m.role === 'user' ? (
              <div
                key={
                  m.server_cursor ??
                  (m.client_turn_id
                    ? `${m.client_turn_id}:${m.role}`
                    : undefined) ??
                  `${m.created_at ?? 'message'}-${i}`
                }
                className="group flex flex-col items-end gap-0.5"
              >
                <div className="flex max-w-[85%] flex-col gap-2 rounded-2xl rounded-tr-sm bg-primary px-3.5 py-2.5 text-sm text-primary-foreground">
                  <ChatAttachments
                    attachments={m.attachments ?? []}
                    contentBase={
                      userScoped && publicId
                        ? `${base}/${publicId}/attachments`
                        : undefined
                    }
                  />
                  {m.content && (
                    <div className="whitespace-pre-wrap">{m.content}</div>
                  )}
                </div>
                <div className="flex items-center gap-1 px-1">
                  {m.created_at && (
                    <TimeAgo
                      date={m.created_at}
                      className="text-[10px] text-muted-foreground"
                    />
                  )}
                  <MessageCopyButton text={messageCopyText(m)} />
                </div>
              </div>
            ) : (
              <div
                key={
                  m.server_cursor ??
                  (m.client_turn_id
                    ? `${m.client_turn_id}:${m.role}`
                    : undefined) ??
                  `${m.created_at ?? 'message'}-${i}`
                }
                className="group flex items-start"
              >
                <div className="min-w-0 flex-1 space-y-1">
                  <div className="min-w-0 space-y-2 rounded-2xl rounded-tl-sm bg-muted/60 px-3.5 py-2.5">
                    <AssistantBody
                      message={m}
                      streaming={liveTurn && isTrailing}
                      activityLabel={activityLabel}
                      turnStartedAt={turnStartedAt}
                      pendingActionBasePath={pendingActionBasePath}
                      conversationBasePath={base}
                      conversationPublicId={publicId}
                      onFix={(text) => void send(text)}
                      onPermissionResolved={pollForReply}
                    />
                  </div>
                  {assistantParts(m).length > 0 && (
                    <div className="flex items-center gap-1 px-1">
                      {m.created_at && (
                        <TimeAgo
                          date={m.created_at}
                          className="text-[10px] text-muted-foreground"
                        />
                      )}
                      <MessageCopyButton text={messageCopyText(m)} />
                    </div>
                  )}
                </div>
              </div>
            )
          })}

          {showTrailingActivityRow && (
            <div className="flex items-start">
              <div className="rounded-2xl rounded-tl-sm bg-muted/60 px-3.5 py-2.5 text-sm text-muted-foreground">
                <TurnActivity label={activityLabel} startedAt={turnStartedAt} />
              </div>
            </div>
          )}
        </div>

        {showJumpToLatest && (
          <div className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center">
            <Button
              type="button"
              size="sm"
              variant="secondary"
              className="pointer-events-auto h-8 gap-1.5 rounded-full px-3 shadow-md dark:shadow-none"
              onClick={scrollTranscriptToBottom}
            >
              <ArrowDown className="size-4 shrink-0" aria-hidden="true" />
              Jump to latest
            </Button>
          </div>
        )}
      </div>

      {liveUpdatesUnavailable && (
        <div
          className="flex items-center gap-2 rounded-md border border-muted-foreground/20 bg-muted/50 px-3 py-2 text-xs text-muted-foreground"
          role="status"
        >
          <Info className="h-3.5 w-3.5 shrink-0" />
          <span>
            Live updates are unavailable. Completed replies will appear here.
          </span>
        </div>
      )}

      {historyLoadError && visible.length > 0 && (
        <div
          className="flex items-center justify-between gap-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive"
          role="alert"
        >
          <span>{historyLoadError}</span>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-7"
            onClick={retryConversationHistory}
          >
            Retry
          </Button>
        </div>
      )}

      {error && (
        <div
          className="flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2.5"
          role="alert"
          data-error-code={error.code}
        >
          <Info className="mt-0.5 size-4 shrink-0 text-destructive" />
          <div className="min-w-0 space-y-0.5">
            <p className="text-sm font-medium text-destructive">
              {error.title}
            </p>
            <p className="text-xs leading-relaxed text-muted-foreground">
              {error.detail}
            </p>
          </div>
        </div>
      )}

      {/* Page-context chip: tells the user what page context is attached, and
          lets them toggle whether it's sent with the next message. */}
      {pageContext && (
        <button
          type="button"
          onClick={() => setIncludeContext((v) => !v)}
          className={cn(
            'flex items-center gap-1.5 self-start rounded-full border px-2.5 py-1 text-xs transition-colors',
            includeContext
              ? 'border-primary/30 bg-primary/10 text-primary hover:bg-primary/15'
              : 'border-border bg-muted/40 text-muted-foreground hover:bg-muted'
          )}
          title={
            includeContext
              ? `Context about ${pageContext.label} is shared with the assistant. Click to exclude it.`
              : `Context about ${pageContext.label} is not shared. Click to include it.`
          }
        >
          <Paperclip
            className={cn('h-3 w-3', !includeContext && 'opacity-50')}
          />
          {includeContext
            ? `Sharing context: ${pageContext.label}`
            : `Share context: ${pageContext.label}`}
        </button>
      )}

      <InteractiveToolsNotice
        provider={selectedProvider}
        contextType={contextType}
        permissionMode={runtimeSelection.permissionModeId}
      />
      {providerStatusState === 'loading' && providerOptions.length === 0 && (
        <div
          className="flex items-center gap-2 text-xs text-muted-foreground"
          role="status"
        >
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
          Loading AI providers and models…
        </div>
      )}
      {providerStatusState === 'error' && (
        <div
          className="flex items-center justify-between gap-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive"
          role="alert"
        >
          <span>Couldn’t load AI providers or model capabilities.</span>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-7"
            onClick={() => void loadProviderStatus(true)}
          >
            Retry
          </Button>
        </div>
      )}
      {providerStatusState === 'success' &&
        selectedProviderOption?.model_discovery_status === 'unavailable' && (
          <div
            className="flex items-center justify-between gap-3 rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-700 dark:text-amber-300"
            role="status"
          >
            <span>
              {selectedProviderOption.model_discovery_error ??
                'Model discovery is unavailable; the provider default remains usable.'}
            </span>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-7"
              onClick={() => void loadProviderStatus(true)}
            >
              Retry
            </Button>
          </div>
        )}

      <div className="shrink-0 overflow-hidden rounded-2xl border border-input bg-background focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/20">
        {(pendingAttachments.length > 0 || attachmentUploads > 0) && (
          <div className="flex flex-wrap items-center gap-2 border-b px-3 py-2">
            <ChatAttachments
              attachments={pendingAttachments}
              onRemove={removePendingAttachment}
              contentBase={
                userScoped && publicId
                  ? `${base}/${publicId}/attachments`
                  : undefined
              }
            />
            {attachmentUploads > 0 && (
              <div className="flex items-center gap-1.5 rounded-lg border px-2.5 py-2 text-xs text-muted-foreground">
                <Loader2 className="size-3.5 animate-spin" />
                Uploading {attachmentUploads}
              </div>
            )}
          </div>
        )}
        <Textarea
          ref={composerRef}
          autoFocus
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder={placeholder}
          rows={2}
          disabled={composerDisabled}
          className="min-h-[72px] resize-none overflow-y-hidden rounded-none border-0 shadow-none focus-visible:border-0 focus-visible:outline-none focus-visible:ring-0"
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault()
              submitComposer()
            }
          }}
        />
        <div className="flex items-center justify-between gap-2 border-t px-2 py-1.5 sm:px-3 sm:py-2">
          <div className="flex min-w-0 flex-wrap items-center gap-1">
            <input
              ref={attachmentInputRef}
              type="file"
              multiple
              className="sr-only"
              onChange={(event) => void uploadAttachments(event.target.files)}
            />
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="h-8 w-8 shrink-0 rounded-full"
              disabled={
                !userScoped ||
                !publicId ||
                attachmentUploads > 0 ||
                pendingAttachments.length >= 8
              }
              onClick={() => attachmentInputRef.current?.click()}
              aria-label="Attach files or images"
              title={
                !publicId
                  ? 'Create the workspace thread before attaching files'
                  : 'Attach files or images (20 MB each)'
              }
            >
              <Paperclip className="size-3.5" />
            </Button>
            {providerStatusState === 'loading' ? (
              <div
                className="flex h-8 items-center gap-2 px-2"
                role="status"
                aria-label="Loading AI runtime options"
              >
                <Skeleton className="h-4 w-44 rounded-sm" />
                <Skeleton className="h-4 w-32 rounded-sm" />
                <Skeleton className="h-4 w-20 rounded-sm" />
                <Skeleton className="h-4 w-36 rounded-sm" />
              </div>
            ) : (
              <>
                <Select
                  value={selectedProvider}
                  onValueChange={(providerId) =>
                    setRuntimeSelection(
                      resolveChatRuntimeSelection(providerOptions, providerId)
                    )
                  }
                  disabled={
                    Boolean(publicId) ||
                    turnActive ||
                    starting ||
                    providerOptions.length === 0
                  }
                >
                  <SelectTrigger
                    className="h-8 w-auto max-w-64 rounded-full border-0 bg-transparent px-2 text-xs shadow-none hover:bg-muted"
                    title={
                      publicId
                        ? 'Provider is fixed when a conversation is created'
                        : 'Choose the provider for this new chat'
                    }
                  >
                    {publicId ? (
                      <Lock className="mr-1 h-3 w-3 shrink-0" />
                    ) : null}
                    {selectedProvider && (
                      <AiHarnessLogo
                        providerId={selectedProvider}
                        size={20}
                        className="mr-1"
                      />
                    )}
                    <SelectValue>
                      {selectedProviderOption
                        ? chatProviderLabel(selectedProviderOption)
                        : providerOptions.length === 0
                          ? 'No provider configured'
                          : 'Select provider'}
                    </SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {providerOptions.map((provider) => (
                      <SelectItem key={provider.id} value={provider.id}>
                        <span className="flex items-center gap-2">
                          <AiHarnessLogo providerId={provider.id} size={22} />
                          <span>{chatProviderLabel(provider)}</span>
                        </span>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>

                {providerStatusState === 'success' &&
                  providerOptions.length === 0 && (
                    <Button
                      asChild
                      variant="link"
                      size="sm"
                      className="h-8 px-1 text-xs"
                    >
                      <a
                        href={
                          contextType === 'application'
                            ? '/agent-sandbox/providers'
                            : '/ai-gateway'
                        }
                      >
                        {contextType === 'application'
                          ? 'Configure a harness'
                          : 'Configure an AI provider'}
                      </a>
                    </Button>
                  )}

                {selectedProviderOption &&
                  selectedProviderOption.models.length > 0 && (
                    <SearchableSelect
                      value={runtimeSelection.modelId ?? undefined}
                      onValueChange={(modelId) =>
                        setRuntimeSelection(
                          resolveChatRuntimeSelection(
                            providerOptions,
                            selectedProvider,
                            {
                              modelId,
                              permissionModeId:
                                runtimeSelection.permissionModeId,
                            }
                          )
                        )
                      }
                      options={modelOptions}
                      placeholder="Model"
                      searchPlaceholder="Search models..."
                      emptyText="No matching models."
                      searchMode="contains"
                      disabled={turnActive || starting}
                      icon={<Sparkles className="mr-1 h-3.5 w-3.5 shrink-0" />}
                      title="Choose the model for the next turn"
                      className="h-8 w-auto max-w-56 rounded-full border-0 bg-transparent px-2 text-xs shadow-none hover:bg-muted"
                    />
                  )}

                {selectedModelOption &&
                  (
                    selectedModelOption.tool_thinking_options ??
                    selectedModelOption.thinking_options
                  ).length > 0 && (
                    <Select
                      value={runtimeSelection.thinkingOptionId ?? undefined}
                      onValueChange={(thinkingOptionId) =>
                        setRuntimeSelection((selection) => ({
                          ...selection,
                          thinkingOptionId,
                        }))
                      }
                      disabled={turnActive || starting}
                    >
                      <SelectTrigger
                        className="h-8 w-auto max-w-40 rounded-full border-0 bg-transparent px-2 text-xs shadow-none hover:bg-muted"
                        title={
                          publicId
                            ? 'Choose the reasoning level for the next turn'
                            : 'Choose how much reasoning the model should use'
                        }
                      >
                        <Brain
                          className="mr-1 size-4 shrink-0"
                          aria-hidden="true"
                        />
                        <SelectValue placeholder="Thinking" />
                      </SelectTrigger>
                      <SelectContent>
                        {(
                          selectedModelOption.tool_thinking_options ??
                          selectedModelOption.thinking_options
                        ).map((option) => (
                          <SelectItem key={option.id} value={option.id}>
                            {chatThinkingItemContent(option)}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}

                {selectedProviderOption &&
                  selectedProviderOption.permission_modes.length > 0 && (
                    <Select
                      value={runtimeSelection.permissionModeId ?? undefined}
                      onValueChange={(permissionModeId) =>
                        void changePermissionMode(permissionModeId)
                      }
                      disabled={
                        starting ||
                        (turnActive &&
                          permissionModeIsAuto(
                            runtimeSelection.permissionModeId
                          ))
                      }
                    >
                      <SelectTrigger
                        className="h-8 w-auto max-w-48 rounded-full border-0 bg-transparent px-2 text-xs shadow-none hover:bg-muted"
                        title={
                          turnActive
                            ? 'Switch this running sandbox turn to Auto'
                            : 'Choose the permission mode for the next turn'
                        }
                      >
                        <Shield className="mr-1 h-3.5 w-3.5 shrink-0" />
                        <SelectValue placeholder="Permissions" />
                      </SelectTrigger>
                      <SelectContent>
                        {selectedProviderOption.permission_modes.map((mode) => (
                          <SelectItem
                            key={mode.id}
                            value={mode.id}
                            disabled={permissionModeOptionDisabled(
                              turnActive,
                              mode.id
                            )}
                          >
                            {chatPermissionLabel(mode)}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}

                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8 shrink-0"
                  disabled={providerRefreshing || turnActive || starting}
                  onClick={() => void loadProviderStatus(true)}
                  aria-label="Refresh provider authentication and models"
                  title="Refresh provider authentication and models"
                >
                  <RefreshCw
                    className={cn(
                      'h-3.5 w-3.5',
                      providerRefreshing && 'animate-spin'
                    )}
                  />
                </Button>
              </>
            )}
          </div>
          {turnActive && !input.trim() ? (
            <Button
              type="button"
              onClick={stop}
              size="icon"
              variant="secondary"
              className="rounded-full"
              title="Stop generating"
              aria-label="Stop generating"
            >
              <Square className="h-3.5 w-3.5 fill-current" />
            </Button>
          ) : (
            <Button
              onClick={submitComposer}
              disabled={
                (!input.trim() && pendingAttachments.length === 0) ||
                attachmentUploads > 0 ||
                (!publicId && !lazyCreate) ||
                (!publicId && providerOptions.length === 0)
              }
              size="icon"
              className="rounded-full"
              aria-label={
                streaming ? 'Interrupt and send message' : 'Send message'
              }
              title={streaming ? 'Interrupt and send message' : undefined}
            >
              <Send className="h-4 w-4" />
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}
