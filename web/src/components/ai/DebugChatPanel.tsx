import {
  createConversation,
  findConversation,
  getConversation,
  getProject,
  updateProjectSettings,
} from '@/api/client'
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
import { Textarea } from '@/components/ui/textarea'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  Check,
  ChevronDown,
  ChevronRight,
  Loader2,
  Paperclip,
  Send,
  ShieldCheck,
  Sparkles,
  Square,
  Wrench,
  X,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { useCallback, useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import rehypeHighlight from 'rehype-highlight'
import { useAiAssistant } from './AiAssistantContext'
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
interface ToolCall {
  id: string
  name: string
  arguments: string
  /** undefined while running (live), a string once done; null only from the API. */
  result?: string | null
}

/**
 * One ordered segment of an assistant turn — a chunk of prose or a tool
 * invocation — so tools render inline where they occurred instead of all
 * hoisted above the text. Built in arrival order from the live event stream, and
 * replayed from `metadata.parts` on reload (so live and reload look identical).
 */
type ChatPart =
  | { type: 'text'; text: string }
  | { type: 'tool'; tool: ToolCall }

/**
 * Local chat message shape. Mirrors the SDK's MessageResponse. Assistant turns
 * carry ordered `parts` (text/tool segments); `tools` is kept for backward
 * compatibility with turns persisted before parts were tracked.
 */
interface ChatMessage {
  role: string
  content: string
  created_at?: string
  tools?: ToolCall[]
  parts?: ChatPart[]
}

/** Render segments for an assistant message, falling back for legacy turns that
 *  predate ordered `parts` (tools first, then the prose). */
function assistantParts(m: ChatMessage): ChatPart[] {
  if (m.parts && m.parts.length > 0) return m.parts
  const parts: ChatPart[] = []
  for (const tool of m.tools ?? []) parts.push({ type: 'tool', tool })
  if (m.content) parts.push({ type: 'text', text: m.content })
  return parts
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
        rehypePlugins={[[rehypeHighlight, { detect: true, ignoreMissing: true }]]}
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
  proposed: { label: 'Awaiting your confirmation', cls: 'text-amber-600 dark:text-amber-400' },
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
          <div className={cn('text-[11px] font-medium', st.cls)}>{st.label}</div>
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
                  buildFixMessage(proposal.operation, proposal.method, params, error)
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
      if (statuses.slice(0, i).every((st) => st === 'executed')) actionableIdx = i
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
          result: d?.result != null ? JSON.stringify(d.result) : prev[actionId]?.result,
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
                        buildFixMessage(s.operation, s.method, stt.params ?? null, stt.error ?? null)
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
  if (plan) return <PlanActionCard projectId={projectId} plan={plan} onFix={onFix} />
  return <PendingActionCard projectId={projectId} tool={tool} onFix={onFix} />
}

/**
 * A slim, in-chat affordance to turn on AI *write actions* for this project —
 * shown only while they're off. Enabling is a deliberate, security-sensitive
 * step (it lets the AI PROPOSE mutations), so it goes through a confirmation
 * dialog rather than a bare toggle; every proposed action is still individually
 * confirm-gated at execution time. This removes the trip to Settings without
 * cheapening the opt-in.
 */
function WriteActionsEnabler({ projectId }: { projectId: number }) {
  // null = still loading / unknown; true = on (render nothing); false = off.
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

  const enable = async () => {
    setBusy(true)
    try {
      const { error } = await updateProjectSettings({
        path: { project_id: projectId },
        // Enable the read-only chat too, so a project never ends up with write
        // actions on but the chat itself off (the chat is where you propose and
        // confirm those writes).
        body: { ai_write_actions_enabled: true, ai_debug_chat_enabled: true },
      })
      if (error) throw error
      setEnabled(true)
      setConfirmOpen(false)
      toast.success('AI write actions enabled for this project')
    } catch {
      toast.error(
        "Couldn't enable write actions — you may need project admin permission."
      )
    } finally {
      setBusy(false)
    }
  }

  // Hidden while loading, unknown, or already enabled.
  if (enabled !== false) return null

  return (
    <>
      <button
        type="button"
        onClick={() => setConfirmOpen(true)}
        className="flex w-full items-center gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-2.5 py-1.5 text-left text-xs text-amber-700 transition-colors hover:bg-amber-500/10 dark:text-amber-400"
      >
        <ShieldCheck className="h-3.5 w-3.5 shrink-0" />
        <span className="min-w-0 flex-1">
          Read-only. <span className="font-medium">Enable write actions</span> to
          let the AI propose changes.
        </span>
      </button>
      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Enable AI write actions?</AlertDialogTitle>
            <AlertDialogDescription>
              This lets the assistant <strong>propose</strong> changes to this
              project — redeploys, restarts, environment variables, domains.
              Nothing runs automatically: every proposed action waits for you to
              review and <strong>Confirm</strong> it here in the chat, and runs
              with your own permissions. You can turn this off anytime in
              Settings → Security.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={busy}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                void enable()
              }}
              disabled={busy}
            >
              {busy && <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />}
              Enable write actions
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
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

function AssistantAvatar() {
  return (
    <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-primary/10">
      <Sparkles className="h-4 w-4 text-primary" />
    </div>
  )
}

/** Open links (including `remark-gfm` autolinked bare URLs) in a new tab, styled
 *  as links. `rel="noopener noreferrer"` so the opened page can't access us. */
const markdownComponents: Components = {
  a({ node: _node, className, ...props }) {
    return (
      <a
        {...props}
        target="_blank"
        rel="noopener noreferrer"
        className={cn(
          'font-medium text-primary underline underline-offset-2 hover:text-primary/80 break-all',
          className
        )}
      />
    )
  },
  // Give horizontally-scrolling code blocks a thin, subtle scrollbar instead of
  // the chunky default OS bar over the dark code surface.
  pre({ node: _node, className, ...props }) {
    return <pre {...props} className={cn('scrollbar-thin', className)} />
  },
}

/** Render one chunk of assistant prose as Markdown. */
function MarkdownText({ text }: { text: string }) {
  return (
    <div className={proseClasses}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkSoftBreaks]}
        // `detect` so unlabeled ``` fences (common in LLM output) still get
        // highlighted; `ignoreMissing` avoids throwing on an unknown language hint.
        rehypePlugins={[[rehypeHighlight, { detect: true, ignoreMissing: true }]]}
        components={markdownComponents}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
}

/**
 * The body of an assistant turn: its ordered text/tool segments, so a tool card
 * shows exactly where the model invoked it rather than hoisted above the prose.
 * Shows the typing indicator only while a trailing turn is streaming with nothing
 * rendered yet (no tools, no text) — never an empty turn left by a failed send.
 */
function AssistantBody({
  message,
  streaming,
  projectId,
  onFix,
}: {
  message: ChatMessage
  streaming: boolean
  projectId: number
  onFix?: (text: string) => void
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
        ) : (
          <MarkdownText key={`text-${idx}`} text={part.text} />
        )
      )}
    </>
  )
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
  const [messages, setMessages] = useState<ChatMessage[]>([])
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

  const stop = useCallback(() => {
    abortRef.current?.abort()
  }, [])

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
      // Pop the trailing optimistic assistant turn if it never received content
      // (used on every failure path so a failed send leaves only the error line).
      const dropEmptyAssistantTurn = () =>
        setMessages((m) => {
          const last = m[m.length - 1]
          return last?.role === 'assistant' && last.content === ''
            ? m.slice(0, -1)
            : m
        })
      try {
        // Lazy-create the conversation on the first message (new project chat).
        if (!id) {
          const { data: conv, error: problem } = await createConversation({
            path: { project_id: projectId },
            body: { context_type: contextType, context_id: ctxId },
          })
          if (!conv) {
            setError(
              (problem as { detail?: string } | undefined)?.detail ||
                'Could not start the chat. Make sure an AI provider is configured.'
            )
            dropEmptyAssistantTurn()
            return
          }
          id = conv.public_id
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
          dropEmptyAssistantTurn()
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
            if (eventName === 'error') {
              if (chunk) setError(chunk)
              dropEmptyAssistantTurn()
              continue
            }
            if (eventName === 'tool_call') {
              try {
                const t = JSON.parse(chunk) as {
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
              continue
            }
            if (eventName === 'tool_result') {
              try {
                const t = JSON.parse(chunk) as {
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
              continue
            }
            if (chunk) {
              setMessages((m) => {
                const copy = [...m]
                const last = copy[copy.length - 1]
                // Append to the trailing text part, or open a new one (so prose
                // that follows a tool call becomes its own segment).
                const prevParts = last?.parts ?? []
                const lastPart = prevParts[prevParts.length - 1]
                const parts: ChatPart[] =
                  lastPart?.type === 'text'
                    ? [
                        ...prevParts.slice(0, -1),
                        { type: 'text', text: lastPart.text + chunk },
                      ]
                    : [...prevParts, { type: 'text', text: chunk }]
                copy[copy.length - 1] = {
                  ...last,
                  role: 'assistant',
                  content: (last?.content ?? '') + chunk,
                  parts,
                  created_at: last?.created_at ?? new Date().toISOString(),
                }
                return copy
              })
            }
          }
        }
      } catch (e) {
        // A user-initiated Stop (AbortController) is not an error — just keep
        // whatever streamed so far and drop the turn only if nothing arrived.
        if (e instanceof DOMException && e.name === 'AbortError') {
          dropEmptyAssistantTurn()
        } else {
          setError('Connection error while talking to the AI.')
          dropEmptyAssistantTurn()
        }
      } finally {
        abortRef.current = null
        setStreaming(false)
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
    ]
  )

  const start = useCallback(async () => {
    setStarting(true)
    setError(null)
    try {
      const { data: conv, error: problem } = await createConversation({
        path: { project_id: projectId },
        body: { context_type: contextType, context_id: ctxId },
      })
      if (!conv) {
        setError(
          (problem as { detail?: string } | undefined)?.detail ||
            'Could not start the chat. Make sure an AI provider is configured.'
        )
        return
      }
      setPublicId(conv.public_id)
      setMessages([])
      void send(startPrompt, conv.public_id)
    } catch {
      setError('Could not start the chat.')
    } finally {
      setStarting(false)
    }
  }, [projectId, contextType, ctxId, startPrompt, send])

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
        setPublicId(conv.public_id)
        const { data: detail } = await getConversation({
          path: { project_id: projectId, public_id: conv.public_id },
        }).catch(() => ({ data: null }))
        if (!ignore && detail) {
          setMessages(
            (detail.messages ?? []).map((m) => {
              // `parts` is newer than the current generated SDK type; read it
              // defensively until the client is regenerated.
              const rawParts = (m as { parts?: ChatPart[] }).parts
              return {
                role: m.role,
                content: m.content,
                created_at: m.created_at,
                tools: m.tools ?? undefined,
                parts:
                  rawParts && rawParts.length > 0 ? rawParts : undefined,
              }
            })
          )
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

  // Report the active conversation id upward (lets the dock reset it).
  useEffect(() => {
    onConversationChange?.(publicId)
  }, [publicId, onConversationChange])

  const visible = messages.filter((m) => m.role !== 'system')
  const busy = streaming || starting
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
              <Button onClick={() => void start()} disabled={starting}>
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
          <div className="flex items-start gap-2.5">
            <AssistantAvatar />
            <div className="flex items-center gap-2 rounded-2xl rounded-tl-sm bg-muted/60 px-3.5 py-2.5 text-sm text-muted-foreground">
              <TypingDots />
              Reading logs and analyzing the failure…
            </div>
          </div>
        )}

        {visible.map((m, i) =>
          m.role === 'user' ? (
            <div key={i} className="flex flex-col items-end gap-0.5">
              <div className="max-w-[85%] whitespace-pre-wrap rounded-2xl rounded-tr-sm bg-primary px-3.5 py-2.5 text-sm text-primary-foreground">
                {m.content}
              </div>
              {m.created_at && (
                <TimeAgo
                  date={m.created_at}
                  className="px-1 text-[10px] text-muted-foreground"
                />
              )}
            </div>
          ) : (
            <div key={i} className="flex items-start gap-2.5">
              <AssistantAvatar />
              <div className="min-w-0 flex-1 space-y-1">
                <div className="min-w-0 space-y-2 rounded-2xl rounded-tl-sm bg-muted/60 px-3.5 py-2.5">
                  <AssistantBody
                    message={m}
                    streaming={streaming && i === visible.length - 1}
                    projectId={projectId}
                    onFix={(text) => void send(text)}
                  />
                </div>
                {m.created_at && assistantParts(m).length > 0 && (
                  <TimeAgo
                    date={m.created_at}
                    className="px-1 text-[10px] text-muted-foreground"
                  />
                )}
              </div>
            </div>
          )
        )}
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

      <WriteActionsEnabler projectId={projectId} />

      <div className="flex items-end gap-2 border-t pt-3">
        <Textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder={placeholder}
          rows={2}
          disabled={streaming || (!publicId && !starting && !lazyCreate)}
          className="resize-none"
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault()
              void send(input)
            }
          }}
        />
        {streaming ? (
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
            onClick={() => void send(input)}
            disabled={!input.trim() || (!publicId && !lazyCreate)}
            size="icon"
            aria-label="Send message"
          >
            <Send className="h-4 w-4" />
          </Button>
        )}
      </div>
    </div>
  )
}
