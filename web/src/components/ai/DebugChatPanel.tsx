// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createConversation,
  findConversation,
  getAiProviderStatus,
  getConversation,
  getProject,
  listPendingActions,
  refreshAiProviderStatus,
  type PendingActionResponse,
  updateProjectSettings,
} from '@/api/client'
import {
  PermissionCard,
  type PermissionRequest,
} from '@/components/ai/PermissionCard'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
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
  Bot,
  Brain,
  ChevronDown,
  ChevronRight,
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
  chatProviderLabel,
  reconcileChatRuntimeAfterRefresh,
  resolveChatRuntimeSelection,
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
  unrepresentedPendingActions,
  type ChatMessage,
  type ChatPart,
  type ToolCall,
} from './chat-message-parts'
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

/**
 * Maps a `getConversation` response into the panel's `ChatMessage[]` state,
 * including a still-unresolved permission request (ADR-038 Phase 2) as a live,
 * answerable card rather than the inert "asked" text alone — used both on
 * initial load and by `pollForReply`'s fallback refetch.
 */
function mapConversationDetail(detail: {
  messages?: Array<{
    role: string
    content: string
    created_at?: string
    tools?: ChatMessage['tools'] | null
    parts?: ChatPart[] | null
  }> | null
  pending_permission?: PermissionRequest | null
}): ChatMessage[] {
  const mapped: ChatMessage[] = (detail.messages ?? []).map((m) => {
    const rawParts = m.parts
    return {
      role: m.role,
      content: m.content,
      created_at: m.created_at,
      tools: m.tools ?? undefined,
      parts: rawParts && rawParts.length > 0 ? rawParts : undefined,
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

/**
 * Pop the trailing optimistic assistant turn if it never received anything.
 * Checking `content` alone isn't enough: a pending permission card lives in
 * `parts`, not `content` — dropping the turn on a dead connection would
 * silently discard a still-answerable question with no way to resolve it
 * (ADR-038 Phase 2).
 */
type SetMessages = React.Dispatch<React.SetStateAction<ChatMessage[]>>
type SetError = React.Dispatch<React.SetStateAction<string | null>>

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

/**
 * Apply one wire event (SSE frame or WebSocket frame — both carry the same
 * `(eventName, data)` shape, tee'd from the same point server-side) to the
 * trailing assistant turn. Shared by the sending tab's own fetch-reader loop
 * and the cross-tab `useConversationStream` WS listener, so token/tool/
 * permission rendering can never drift between "I sent this" and "I'm
 * watching this."
 */
function applyWireEvent(
  eventName: string,
  data: string,
  setMessages: SetMessages,
  setError: SetError
) {
  if (eventName === 'error') {
    if (data) setError(data)
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
  // Plain token text (unnamed SSE frame).
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
function toolLabel(tool: ToolCall): string {
  if (tool.name === 'temps' || tool.name === 'temps_write') {
    try {
      const args = JSON.parse(tool.arguments) as { command?: unknown }
      if (typeof args.command === 'string' && args.command.trim()) {
        return args.command.trim()
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
  return (
    <div className="min-w-0 overflow-hidden rounded-lg border bg-muted/40 text-xs">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex w-full min-w-0 items-center gap-2 px-2.5 py-1.5 text-left transition-colors hover:bg-muted/70"
        aria-expanded={open}
      >
        <Wrench className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate font-mono text-[11px] font-medium">
          {toolLabel(tool)}
        </span>
        {running && (
          <Loader2
            className="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground"
            aria-label="Running"
          />
        )}
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
  projectId,
  tool,
  onFix,
}: {
  projectId: number
  tool: ToolCall
  /** Send a follow-up chat message (used by "Fix with AI" on failure). */
  onFix?: (text: string) => void
}) {
  const proposal = parseProposal(tool.result)
  const actionId = proposal?.action_id
  const [status, setStatus] = useState('proposed')
  const [busy, setBusy] = useState<'confirm' | 'reject' | null>(null)
  const [result, setResult] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  // The exact request params/body that will be sent, redacted server-side
  // (value/secret/password/token/key → ***). Shown so the user can review what
  // the action will actually do before confirming.
  const [params, setParams] = useState<string | null>(null)
  const [open, setOpen] = useState(false)

  // Reconcile the live status once the action id is known (covers reloads).
  useEffect(() => {
    if (!actionId) return
    let cancelled = false
    fetch(`/api/projects/${projectId}/ai/pending-actions/${actionId}`, {
      credentials: 'include',
    })
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => {
        if (cancelled || !d) return
        if (typeof d.status === 'string') setStatus(d.status)
        if (d.result != null) setResult(JSON.stringify(d.result))
        if (typeof d.error === 'string') setError(d.error)
        if (d.params != null) setParams(JSON.stringify(d.params))
      })
      .catch(() => {
        /* status reconcile is best-effort */
      })
    return () => {
      cancelled = true
    }
  }, [projectId, actionId])

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
        `/api/projects/${projectId}/ai/pending-actions/${proposal.action_id}/${kind}`,
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
        if (typeof d.status === 'string') setStatus(d.status)
        if (d.result != null) setResult(JSON.stringify(d.result))
        if (typeof d.error === 'string') setError(d.error)
      }
    } catch {
      setError(`Could not ${kind} the action.`)
    } finally {
      setBusy(null)
    }
  }

  const st = ACTION_STATUS[status] ?? ACTION_STATUS.proposed
  const pending = status === 'proposed'
  return (
    <div className="min-w-0 overflow-hidden rounded-lg border border-amber-500/30 bg-amber-500/5 text-xs">
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
      {params && params !== '{}' && (
        <div className="min-w-0 space-y-1 border-t border-amber-500/20 px-2.5 py-2">
          <div className="font-medium text-muted-foreground">
            {pending ? 'Will send' : 'Sent'}
          </div>
          <ToolBlock value={params} />
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
  projectId,
  plan,
  onFix,
}: {
  projectId: number
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
        fetch(`/api/projects/${projectId}/ai/pending-actions/${s.action_id}`, {
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
  }, [projectId, plan])

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
      const r = await fetch(
        `/api/projects/${projectId}/ai/pending-actions/${actionId}/${kind}`,
        {
          method: 'POST',
          credentials: 'include',
          headers: { 'Content-Type': 'application/json' },
        }
      )
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
  projectId,
  tool,
  onFix,
}: {
  projectId: number
  tool: ToolCall
  onFix?: (text: string) => void
}) {
  const plan = parsePlanProposal(tool.result)
  if (plan)
    return <PlanActionCard projectId={projectId} plan={plan} onFix={onFix} />
  return <PendingActionCard projectId={projectId} tool={tool} onFix={onFix} />
}

/** A visible, reversible project-level control for Temps write proposals. */
function WriteActionsEnabler({ projectId }: { projectId: number }) {
  // null = still loading / unknown.
  const [enabled, setEnabled] = useState<boolean | null>(null)
  const [confirmOpen, setConfirmOpen] = useState(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    let cancelled = false
    getProject({ path: { id: projectId } })
      .then(({ data }) => {
        if (!cancelled && data) {
          setEnabled(data.ai_write_actions_enabled === true)
        }
      })
      .catch(() => {
        /* leave unknown — just don't show the affordance */
      })
    return () => {
      cancelled = true
    }
  }, [projectId])

  const update = async (nextEnabled: boolean) => {
    setBusy(true)
    try {
      const { error } = await updateProjectSettings({
        path: { project_id: projectId },
        // Enabling writes also enables chat, because chat is where proposals
        // are reviewed. Disabling writes must not unexpectedly disable chat.
        body: {
          ai_write_actions_enabled: nextEnabled,
          ...(nextEnabled ? { ai_debug_chat_enabled: true } : {}),
        },
      })
      if (error) throw error
      setEnabled(nextEnabled)
      setConfirmOpen(false)
      toast.success(
        nextEnabled
          ? 'AI write proposals enabled for this project'
          : 'AI write proposals disabled for this project'
      )
    } catch {
      toast.error(
        `Couldn't ${nextEnabled ? 'enable' : 'disable'} write proposals — you may need project admin permission.`
      )
    } finally {
      setBusy(false)
    }
  }

  if (enabled === null) {
    return (
      <Skeleton
        className="h-8 w-48 rounded-md"
        aria-label="Loading AI write status"
      />
    )
  }

  return (
    <>
      <button
        type="button"
        onClick={() => setConfirmOpen(true)}
        className={cn(
          'flex w-full items-center gap-2 rounded-md border px-2.5 py-1.5 text-left text-xs transition-colors',
          enabled
            ? 'border-green-500/30 bg-green-500/5 text-green-700 hover:bg-green-500/10 dark:text-green-400'
            : 'border-amber-500/30 bg-amber-500/5 text-amber-700 hover:bg-amber-500/10 dark:text-amber-400'
        )}
      >
        {enabled ? (
          <ShieldCheck className="h-3.5 w-3.5 shrink-0" />
        ) : (
          <Shield className="h-3.5 w-3.5 shrink-0" />
        )}
        <span className="min-w-0 flex-1">
          {enabled ? (
            <>
              <span className="font-medium">Write proposals enabled.</span>{' '}
              Every action still requires confirmation.
            </>
          ) : (
            <>
              Read-only.{' '}
              <span className="font-medium">Enable write proposals</span> to let
              the AI suggest changes.
            </>
          )}
        </span>
      </button>
      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {enabled
                ? 'Disable AI write proposals?'
                : 'Enable AI write proposals?'}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {enabled ? (
                <>
                  The assistant will return to read-only access and cannot stage
                  new changes. Existing proposals remain available for you to
                  confirm or reject.
                </>
              ) : (
                <>
                  This lets the assistant <strong>propose</strong> changes to
                  this project — redeploys, restarts, environment variables,
                  domains. Nothing runs automatically: every proposal waits for
                  you to review and <strong>Confirm</strong> it here in chat,
                  and runs with your own permissions.
                </>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={busy}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                void update(!enabled)
              }}
              disabled={busy}
            >
              {busy && <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />}
              {enabled ? 'Disable write proposals' : 'Enable write proposals'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
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
function InteractiveToolsNotice({ provider }: { provider: string }) {
  // null = still loading / unknown.
  const [supported, setSupported] = useState<boolean | null>(null)
  // `interactive_bridge_status` from the extended provider status response.
  // Not in the generated types yet, so we read it defensively.
  const [bridgeStatus, setBridgeStatus] = useState<string | null | undefined>(
    undefined
  )

  useEffect(() => {
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
  }, [])

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
  projectId: number
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

const proseClasses =
  'prose prose-sm dark:prose-invert max-w-none prose-pre:bg-[#0d1117] prose-pre:text-xs prose-pre:border-0 prose-pre:overflow-x-auto prose-pre:rounded-lg prose-code:before:content-none prose-code:after:content-none prose-p:my-1.5 prose-headings:my-2 prose-ul:my-1.5 prose-ul:list-disc prose-ul:pl-5 prose-ol:my-1.5 prose-ol:list-decimal prose-ol:pl-5 prose-li:my-0.5 prose-li:marker:text-foreground/60 prose-hr:my-3 prose-hr:border-border prose-table:text-xs prose-th:px-2 prose-th:py-1 prose-td:px-2 prose-td:py-1'

/** Three-dot "assistant is thinking" indicator. */
function TypingDots() {
  return (
    <span className="inline-flex items-center gap-1 py-1" aria-label="Thinking">
      <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-muted-foreground/70 [animation-delay:-0.3s]" />
      <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-muted-foreground/70 [animation-delay:-0.15s]" />
      <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-muted-foreground/70" />
    </span>
  )
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
 * prose. Shows the typing indicator only while a trailing turn is streaming
 * with nothing rendered yet.
 */
function AssistantBody({
  message,
  streaming,
  projectId,
  conversationPublicId,
  onFix,
  onPermissionResolved,
}: {
  message: ChatMessage
  streaming: boolean
  projectId: number
  conversationPublicId: string | null
  onFix?: (text: string) => void
  onPermissionResolved?: () => void
}) {
  const parts = assistantParts(message)
  if (parts.length === 0) {
    return streaming ? <TypingDots /> : null
  }
  return (
    <>
      {parts.map((part, idx) =>
        part.type === 'tool' ? (
          // A `temps_write` tool is a *proposed* mutation — render the human
          // confirm/reject gate instead of a read-only result card.
          part.tool.name === 'temps_write' ? (
            <WriteProposalCard
              key={part.tool.id}
              projectId={projectId}
              tool={part.tool}
              onFix={onFix}
            />
          ) : (
            <ToolCard key={part.tool.id} tool={part.tool} />
          )
        ) : part.type === 'permission' ? (
          // ADR-038 Phase 2: interactive bridge permission request
          <PermissionCard
            key={`perm-${part.permission.id}`}
            projectId={projectId}
            conversationPublicId={conversationPublicId ?? ''}
            permission={part.permission}
            onResolved={onPermissionResolved}
          />
        ) : (
          <MarkdownText key={`text-${idx}`} text={part.text} />
        )
      )}
    </>
  )
}

/** Backoff schedule for WS reconnects — bounded, no thundering herd. */
const WS_RECONNECT_DELAYS_MS = [1000, 2000, 5000, 10000, 15000]

/**
 * Keeps a `GET .../conversations/{publicId}/stream` WebSocket open for the
 * panel's full lifetime (not just during an active send, unlike the fetch
 * reader in `send()`), so a second tab/device watching the same conversation
 * sees tokens, tool activity, and permission requests live instead of only on
 * reload.
 *
 * `suppressRef` is a counter, not a boolean: the tab that is itself sending a
 * message or resolving a permission must ignore its own broadcast echo (it
 * already has that data from its own request), but those two actions can
 * overlap so a simple flag would race. Once `suppressRef.current` returns to
 * 0 this tab is back to being a normal observer.
 */
function useConversationStream(
  projectId: number,
  publicId: string | null,
  suppressRef: { current: number },
  setMessages: SetMessages,
  setError: SetError,
  setWsTurnActive: React.Dispatch<React.SetStateAction<boolean>>
) {
  useEffect(() => {
    if (!publicId) return
    let cancelled = false
    let ws: WebSocket | null = null
    let attempt = 0
    // True once this effect instance has completed at least one connection —
    // distinguishes the initial connect (history was already loaded by the
    // panel's own init fetch, no need to resync) from a later reconnect
    // (missed whatever happened while disconnected, including possibly a
    // `turn_complete` — resync and clear any stuck "thinking" state rather
    // than trust stale local state).
    let hasConnectedBefore = false
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null

    const resync = () => {
      getConversation({ path: { project_id: projectId, public_id: publicId } })
        .then(({ data }) => {
          if (!cancelled && data) setMessages(mapConversationDetail(data))
        })
        .catch(() => {
          /* best-effort resync — the next event or a manual reload recovers */
        })
    }

    const connect = () => {
      if (cancelled) return
      const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
      const socket = new WebSocket(
        `${wsProtocol}//${window.location.host}/api/projects/${projectId}/ai/conversations/${publicId}/stream`
      )
      ws = socket
      socket.onopen = () => {
        attempt = 0
        if (hasConnectedBefore) {
          setWsTurnActive(false)
          resync()
        }
        hasConnectedBefore = true
      }
      socket.onmessage = (ev) => {
        if (suppressRef.current > 0) return
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
        if (eventName === 'resync_required') {
          setWsTurnActive(false)
          resync()
          return
        }
        if (eventName === 'turn_complete') {
          setWsTurnActive(false)
          return
        }
        if (eventName === 'error') {
          setWsTurnActive(false)
          applyWireEvent(eventName, data, setMessages, setError)
          return
        }
        if (eventName === 'user_message') {
          setWsTurnActive(true)
          try {
            const u = JSON.parse(data) as {
              content: string
              created_at?: string
            }
            const now = u.created_at ?? new Date().toISOString()
            setMessages((m) => [
              ...m,
              { role: 'user', content: u.content, created_at: now },
              { role: 'assistant', content: '', created_at: now },
            ])
          } catch {
            /* ignore malformed user_message frame */
          }
          return
        }
        applyWireEvent(eventName, data, setMessages, setError)
      }
      socket.onclose = () => {
        if (cancelled) return
        const delay =
          WS_RECONNECT_DELAYS_MS[
            Math.min(attempt, WS_RECONNECT_DELAYS_MS.length - 1)
          ]
        attempt += 1
        reconnectTimer = setTimeout(connect, delay)
      }
      socket.onerror = () => {
        socket.close()
      }
    }

    connect()
    return () => {
      cancelled = true
      if (reconnectTimer) clearTimeout(reconnectTimer)
      ws?.close()
    }
  }, [projectId, publicId, suppressRef, setMessages, setError, setWsTurnActive])
}

/**
 * The body of the AI debugging chat attached to any entity (ADR-023). Renders a
 * scrollable message list that fills its parent plus a follow-up composer — no
 * surrounding card, so it can drop into a sidebar/sheet or a page section. The
 * streaming reply is consumed via a manual SSE fetch reader (the generated SDK
 * can't stream); find/create/history go through the generated SDK.
 */
export function DebugChatPanel({
  projectId,
  contextType,
  contextId,
  startPrompt = 'Diagnose this and suggest concrete next steps.',
  autoStart = false,
  placeholder = 'Ask a follow-up…',
  lazyCreate = false,
  emptyHint = 'Ask anything about this project.',
  onConversationChange,
}: DebugChatPanelProps) {
  const base = `/api/projects/${projectId}/ai/conversations`
  const ctxId = String(contextId)
  // Per-chat draft key: a half-typed message survives closing the dock,
  // switching chats, and reloads.
  const draftKey = `temps.ai.draft.${projectId}:${contextType}:${ctxId}`
  // Current page context (what the user is viewing). Shown as a chip by the
  // input; the user can toggle whether it's attached.
  const { pageContext } = useAiAssistant()
  const [includeContext, setIncludeContext] = useState(true)
  const [publicId, setPublicId] = useState<string | null>(null)
  const providerPinnedRef = useRef(false)
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
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [pendingActionSnapshot, setPendingActionSnapshot] = useState<{
    conversationId: string
    actions: PendingActionResponse[]
  } | null>(null)
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
  const [error, setError] = useState<string | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  // Aborts the in-flight streaming request when the user hits Stop (or the panel
  // unmounts). Dropping the SSE connection also tells the server to stop
  // generating, so a stopped turn doesn't keep costing tokens.
  const abortRef = useRef<AbortController | null>(null)
  const queuedInterruptRef = useRef<string | null>(null)
  const sendAfterInterruptRef = useRef<(text: string) => void>(() => {})
  // Counts this tab's own in-flight writes to the conversation (an active
  // send, or a permission being resolved+polled) — see useConversationStream.
  const wsSuppressRef = useRef(0)
  // Whether a turn is in flight on ANOTHER tab, per the live wire — an
  // observer tab has no local `streaming` state to drive the "thinking"
  // indicator with, since it never called send() itself.
  const [wsTurnActive, setWsTurnActive] = useState(false)

  // New conversations may choose any provider that is ready on this host.
  // Once the first message creates the row, `publicId` locks only the provider
  // harness. Model, reasoning, and permission mode remain turn-level controls.
  const loadProviderStatus = useCallback(async (forceRefresh = false) => {
    if (forceRefresh) setProviderRefreshing(true)
    else setProviderStatusState('loading')
    try {
      const statusResult = forceRefresh
        ? await refreshAiProviderStatus({ throwOnError: true })
        : await getAiProviderStatus({ throwOnError: true })
      const status = statusResult.data
      const options = (status?.available_providers ?? []).map((provider) => {
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

      const active =
        status?.active_provider_type === 'agent_cli'
          ? status.agent_cli_provider_id
          : 'gateway'
      if (
        !providerPinnedRef.current &&
        active &&
        options.some((option) => option.id === active)
      ) {
        setRuntimeSelection(resolveChatRuntimeSelection(options, active))
      } else if (!providerPinnedRef.current && options[0]) {
        setRuntimeSelection(resolveChatRuntimeSelection(options, options[0].id))
      }
      setProviderStatusState('success')
    } catch {
      if (forceRefresh) {
        toast.error('Couldn’t refresh provider authentication and models')
      } else {
        setProviderStatusState('error')
      }
    } finally {
      setProviderRefreshing(false)
    }
  }, [])

  useEffect(() => {
    // Defer the async refresh out of the effect's synchronous phase. The
    // initial state already renders the loading skeleton, so no paint is lost.
    const timer = window.setTimeout(() => void loadProviderStatus(), 0)
    return () => window.clearTimeout(timer)
  }, [loadProviderStatus])

  useConversationStream(
    projectId,
    publicId,
    wsSuppressRef,
    setMessages,
    setError,
    setWsTurnActive
  )

  // Pending actions are durable rows. Linking their proposal receipt back to
  // an assistant message is deliberately best-effort, so load the rows too:
  // otherwise a transient persistence failure can leave a real proposal with
  // no Confirm/Reject UI after a reload.
  useEffect(() => {
    if (!publicId) return
    if (streaming) return
    let cancelled = false
    listPendingActions({
      path: { project_id: projectId, public_id: publicId },
    })
      .then(({ data }) => {
        if (!cancelled) {
          setPendingActionSnapshot({
            conversationId: publicId,
            actions: data ?? [],
          })
        }
      })
      .catch(() => {
        // The transcript remains usable; an ordinary API error banner would
        // obscure the composer. The next completed turn retries this query.
      })
    return () => {
      cancelled = true
    }
  }, [messages.length, projectId, publicId, streaming])

  const recoveredPendingActions =
    publicId && pendingActionSnapshot?.conversationId === publicId
      ? unrepresentedPendingActions(messages, pendingActionSnapshot.actions)
      : []

  const stop = useCallback(() => {
    abortRef.current?.abort()
  }, [])

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

  // Abort any in-flight stream if the panel unmounts mid-generation.
  useEffect(() => () => abortRef.current?.abort(), [])

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
      // Need either an existing conversation or permission to create one lazily.
      if (!content || (!id && !lazyCreate)) return
      setInput('')
      setError(null)
      setStreaming(true)
      // This tab already gets its own tokens via this request's SSE body — the
      // WS listener must ignore its own broadcast echo of the same turn, or
      // every token would render twice.
      wsSuppressRef.current += 1
      // Optimistically append the user's turn + an empty assistant turn that the
      // stream fills in. The empty assistant turn renders a typing indicator
      // while streaming; on any failure below we drop it again so it can't linger
      // as a perpetual fake "typing" bubble next to the error message.
      const now = new Date().toISOString()
      setMessages((m) => [
        ...m,
        { role: 'user', content, created_at: now },
        { role: 'assistant', content: '', created_at: now },
      ])
      try {
        // Lazy-create the conversation on the first message (new project chat).
        if (!id) {
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
            setError(
              (problem as { detail?: string } | undefined)?.detail ||
                'Could not start the chat. Make sure an AI provider is configured.'
            )
            dropEmptyAssistantTurn(setMessages)
            return
          }
          id = conv.public_id
          providerPinnedRef.current = true
          setPublicId(conv.public_id)
        }
        const controller = new AbortController()
        abortRef.current = controller
        const res = await fetch(`${base}/${id}/messages`, {
          method: 'POST',
          credentials: 'include',
          signal: controller.signal,
          headers: {
            'Content-Type': 'application/json',
            Accept: 'text/event-stream',
          },
          body: JSON.stringify({
            content,
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
        if (!res.ok || !res.body) {
          const problem = await res.json().catch(() => ({}))
          setError(problem.detail || 'The AI request failed.')
          dropEmptyAssistantTurn(setMessages)
          return
        }
        const reader = res.body.getReader()
        const decoder = new TextDecoder()
        let buffer = ''
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          buffer += decoder.decode(value, { stream: true })
          let boundary
          while ((boundary = buffer.indexOf('\n\n')) >= 0) {
            const rawEvent = buffer.slice(0, boundary)
            buffer = buffer.slice(boundary + 2)
            let eventName = ''
            const dataParts: string[] = []
            for (const line of rawEvent.split('\n')) {
              if (line.startsWith('event:')) {
                eventName = line.slice(6).trim()
              } else if (line.startsWith('data:')) {
                dataParts.push(line.slice(5).replace(/^ /, ''))
              }
            }
            const chunk = dataParts.join('\n')
            applyWireEvent(eventName, chunk, setMessages, setError)
          }
        }
      } catch (e) {
        // A user-initiated Stop (AbortController) is not an error — just keep
        // whatever streamed so far and drop the turn only if nothing arrived.
        if (e instanceof DOMException && e.name === 'AbortError') {
          dropEmptyAssistantTurn(setMessages)
        } else {
          setError('Connection error while talking to the AI.')
          dropEmptyAssistantTurn(setMessages)
        }
      } finally {
        abortRef.current = null
        setStreaming(false)
        wsSuppressRef.current = Math.max(0, wsSuppressRef.current - 1)
        const queuedInterrupt = queuedInterruptRef.current
        queuedInterruptRef.current = null
        if (queuedInterrupt) {
          window.setTimeout(
            () => sendAfterInterruptRef.current(queuedInterrupt),
            0
          )
        }
      }
    },
    [
      base,
      publicId,
      lazyCreate,
      projectId,
      contextType,
      ctxId,
      pageContext,
      includeContext,
      selectedProvider,
      runtimeSelection.modelId,
      runtimeSelection.thinkingOptionId,
      runtimeSelection.permissionModeId,
    ]
  )

  sendAfterInterruptRef.current = (text) => void send(text)
  const submitComposer = useCallback(() => {
    const action = chatComposerSubmitAction(input, streaming)
    if (action === 'none') return
    if (action === 'interrupt-and-send') {
      queuedInterruptRef.current = input.trim()
      setInput('')
      stop()
      return
    }
    void send(input)
  }, [input, streaming, send, stop])

  const start = useCallback(async () => {
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
        setError(
          (problem as { detail?: string } | undefined)?.detail ||
            'Could not start the chat. Make sure an AI provider is configured.'
        )
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
      void send(startPrompt, conv.public_id)
    } catch {
      setError('Could not start the chat.')
    } finally {
      setStarting(false)
    }
  }, [
    projectId,
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
  const initialised = useRef(false)
  useEffect(() => {
    if (initialised.current) return
    initialised.current = true
    let ignore = false
    ;(async () => {
      try {
        const { data: conv } = await findConversation({
          path: { project_id: projectId },
          query: { context_type: contextType, context_id: ctxId },
        })
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
        const { data: detail } = await getConversation({
          path: { project_id: projectId, public_id: conv.public_id },
        }).catch(() => ({ data: null }))
        if (!ignore && detail) {
          setMessages(mapConversationDetail(detail))
        }
      } catch {
        /* best-effort: leave the panel in its empty state */
      } finally {
        if (!ignore) setInitializing(false)
      }
    })()
    return () => {
      ignore = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight })
  }, [messages])

  // Fallback for when the SSE connection that was open when a question was
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
      const { data: detail } = await getConversation({
        path: { project_id: projectId, public_id: publicId },
      }).catch(() => ({ data: null }))
      if (cancelled || !detail) {
        release()
        return
      }
      const serverMessageCount = detail.messages?.length ?? 0
      const hasPendingPermission = Boolean(detail.pending_permission)
      let caughtUp = false
      setMessages((m) => {
        if (serverMessageCount <= m.length && !hasPendingPermission) return m
        caughtUp = true
        return mapConversationDetail(detail)
      })
      if (!caughtUp) setTimeout(poll, 2000)
      else release()
    }
    void poll()
    return () => {
      cancelled = true
      release()
    }
  }, [projectId, publicId])

  // Report the active conversation id upward (lets the dock reset it).
  useEffect(() => {
    onConversationChange?.(publicId)
  }, [publicId, onConversationChange])

  const visible = messages.filter((m) => m.role !== 'system')
  const busy = streaming || starting
  // A turn in flight either from this tab's own send() or observed live from
  // another tab over the WS — both need the "thinking" indicator to show.
  const liveTurn = streaming || wsTurnActive
  // Show a standalone "thinking" row only before the optimistic assistant turn
  // exists (i.e. while the conversation is being created).
  const showBootRow = visible.length === 0 && busy

  return (
    <div className="flex h-full min-h-0 flex-col gap-3">
      <div
        ref={scrollRef}
        className="flex-1 min-h-0 space-y-4 overflow-y-auto pr-1"
      >
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

        {!initializing &&
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
              <TypingDots />
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
            <div key={i} className="group flex flex-col items-end gap-0.5">
              <div className="max-w-[85%] whitespace-pre-wrap rounded-2xl rounded-tr-sm bg-primary px-3.5 py-2.5 text-sm text-primary-foreground">
                {m.content}
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
            <div key={i} className="group flex items-start">
              <div className="min-w-0 flex-1 space-y-1">
                <div className="min-w-0 space-y-2 rounded-2xl rounded-tl-sm bg-muted/60 px-3.5 py-2.5">
                  <AssistantBody
                    message={m}
                    streaming={liveTurn && isTrailing}
                    projectId={projectId}
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

        {recoveredPendingActions.map((action) => (
          <div key={action.public_id} className="flex items-start">
            <div className="min-w-0 flex-1 rounded-2xl rounded-tl-sm bg-muted/60 px-3.5 py-2.5">
              <PendingActionCard
                projectId={projectId}
                tool={{
                  id: `pending-${action.public_id}`,
                  name: 'temps_write',
                  arguments: '',
                  result: JSON.stringify({
                    status: 'proposed',
                    action_id: action.public_id,
                    operation: action.operation_id,
                    method: action.method,
                    summary: action.summary,
                  }),
                }}
                onFix={(text) => void send(text)}
              />
            </div>
          </div>
        ))}
      </div>

      {error && <p className="text-sm text-destructive">{error}</p>}

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

      <InteractiveToolsNotice provider={selectedProvider} />
      <WriteActionsEnabler projectId={projectId} />

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

      <div className="shrink-0 overflow-hidden rounded-lg border border-input bg-background transition-[border-color,box-shadow] focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/20">
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
        <div className="flex items-center justify-between gap-2 border-t px-2 py-1.5">
          <div className="flex min-w-0 flex-wrap items-center gap-0.5">
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
                    streaming ||
                    starting ||
                    providerOptions.length === 0
                  }
                >
                  <SelectTrigger
                    className="h-8 w-auto max-w-64 border-0 bg-transparent px-2 text-xs shadow-none"
                    title={
                      publicId
                        ? 'Provider is fixed when a conversation is created'
                        : 'Choose the provider for this new chat'
                    }
                  >
                    {publicId ? (
                      <Lock className="mr-1 h-3 w-3 shrink-0" />
                    ) : (
                      <Bot className="mr-1 h-3.5 w-3.5 shrink-0" />
                    )}
                    <SelectValue
                      placeholder={
                        providerOptions.length === 0
                          ? 'No provider configured'
                          : 'Select provider'
                      }
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {providerOptions.map((provider) => (
                      <SelectItem key={provider.id} value={provider.id}>
                        {chatProviderLabel(provider)}
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
                      <a href="/ai-gateway">Configure an AI provider</a>
                    </Button>
                  )}

                {selectedProviderOption &&
                  selectedProviderOption.models.length > 0 && (
                    <Select
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
                      disabled={streaming || starting}
                    >
                      <SelectTrigger
                        className="h-8 w-auto max-w-56 border-0 bg-transparent px-2 text-xs shadow-none"
                        title="Choose the model for the next turn"
                      >
                        <Sparkles className="mr-1 h-3.5 w-3.5 shrink-0" />
                        <SelectValue placeholder="Model" />
                      </SelectTrigger>
                      <SelectContent>
                        {selectedProviderOption.models.map((model) => (
                          <SelectItem key={model.id} value={model.id}>
                            {model.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}

                {selectedModelOption &&
                  (selectedModelOption.tool_thinking_options ??
                    selectedModelOption.thinking_options).length > 0 && (
                    <Select
                      value={runtimeSelection.thinkingOptionId ?? undefined}
                      onValueChange={(thinkingOptionId) =>
                        setRuntimeSelection((selection) => ({
                          ...selection,
                          thinkingOptionId,
                        }))
                      }
                      disabled={streaming || starting}
                    >
                      <SelectTrigger
                        className="h-8 w-auto max-w-40 border-0 bg-transparent px-2 text-xs shadow-none"
                        title={
                          publicId
                            ? 'Choose the reasoning level for the next turn'
                            : 'Choose how much reasoning the model should use'
                        }
                      >
                        <SelectValue placeholder="Thinking" />
                      </SelectTrigger>
                      <SelectContent>
                        {(selectedModelOption.tool_thinking_options ??
                          selectedModelOption.thinking_options
                        ).map((option) => (
                          <SelectItem key={option.id} value={option.id}>
                            <span className="flex items-center gap-2">
                              <Brain
                                className="h-4 w-4 shrink-0"
                                aria-hidden="true"
                              />
                              <span>{option.name}</span>
                            </span>
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
                        setRuntimeSelection((selection) => ({
                          ...selection,
                          permissionModeId,
                        }))
                      }
                      disabled={streaming || starting}
                    >
                      <SelectTrigger
                        className="h-8 w-auto max-w-48 border-0 bg-transparent px-2 text-xs shadow-none"
                        title="Choose the permission mode for the next turn"
                      >
                        <Shield className="mr-1 h-3.5 w-3.5 shrink-0" />
                        <SelectValue placeholder="Permissions" />
                      </SelectTrigger>
                      <SelectContent>
                        {selectedProviderOption.permission_modes.map((mode) => (
                          <SelectItem key={mode.id} value={mode.id}>
                            {mode.name}
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
                  disabled={providerRefreshing || streaming || starting}
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
          {streaming && !input.trim() ? (
            <Button
              type="button"
              onClick={stop}
              size="icon"
              variant="secondary"
              title="Stop generating"
              aria-label="Stop generating"
            >
              <Square className="h-3.5 w-3.5 fill-current" />
            </Button>
          ) : (
            <Button
              onClick={submitComposer}
              disabled={
                !input.trim() ||
                (!publicId && !lazyCreate) ||
                (!publicId && providerOptions.length === 0)
              }
              size="icon"
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
