/**
 * ADR-038 Phase 2 — PermissionCard
 *
 * Renders a pending permission request emitted by the interactive Claude CLI
 * bridge as a `permission_requested` SSE event. There are three sub-variants
 * by `kind`:
 *
 * - `question`      — AskUserQuestion: one or more questions with option buttons
 * - `plan_approval` — ExitPlanMode: markdown plan + Approve / Reject
 * - `tool_approval` — generic tool call: key/value args + Allow / Deny
 *
 * After the user resolves the card it shows a settled state and becomes
 * non-interactive. On a network error the card shows an inline error and
 * stays interactive so the user can retry.
 *
 * Security: `tool_name` and `input` come from the CLI subprocess and are
 * treated as untrusted. They are rendered only as React children (plain text
 * or `<code>`/`<pre>` elements) — never via `dangerouslySetInnerHTML`.
 */

import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import {
  Check,
  HelpCircle,
  Loader2,
  MapPin,
  Shield,
  X,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import rehypeHighlight from 'rehype-highlight'
import {
  untrustedMarkdownImage,
  untrustedMarkdownLink,
} from '@/components/markdown/untrusted'

// ─── Types ────────────────────────────────────────────────────────────────────

export type PermissionKind = 'tool_approval' | 'question' | 'plan_approval'

export interface PermissionRequest {
  id: string
  kind: PermissionKind
  tool_name: string
  /** Parsed from JSON by the SSE handler; exact shape depends on `kind`. */
  input: unknown
}

// Shape of the `input` field when `kind === 'question'`.
interface QuestionOption {
  label: string
  description?: string
}

interface AskUserQuestionItem {
  question: string
  header?: string
  options: QuestionOption[]
  multiSelect?: boolean
}

interface AskUserQuestionInput {
  questions: AskUserQuestionItem[]
}

// Shape of the `input` field when `kind === 'plan_approval'`.
interface PlanApprovalInput {
  plan?: string
  planFilePath?: string
}

// ─── Resolve helpers ──────────────────────────────────────────────────────────

// Match the Rust PermissionDecision: #[serde(tag = "type", rename_all = "snake_case")]
type PermissionDecision =
  | { type: 'allow_tool' }
  | { type: 'deny_tool'; reason?: string }
  | { type: 'answer_question'; answers: Record<string, string> }
  | { type: 'approve_plan' }
  | { type: 'reject_plan'; feedback?: string }

async function resolvePermission(
  projectId: number,
  publicId: string,
  permissionId: string,
  decision: PermissionDecision
): Promise<void> {
  const res = await fetch(
    `/api/projects/${projectId}/ai/conversations/${publicId}/permissions/${permissionId}/resolve`,
    {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ decision }),
    }
  )
  if (!res.ok) {
    const body = await res.json().catch(() => null)
    const detail =
      (body as { detail?: string } | null)?.detail ??
      `Resolve failed (HTTP ${res.status})`
    throw new Error(detail)
  }
}

// ─── Prose classes (mirrors DebugChatPanel) ───────────────────────────────────

const proseClasses =
  'prose prose-sm dark:prose-invert max-w-none prose-pre:bg-[#0d1117] prose-pre:text-xs prose-pre:border-0 prose-pre:overflow-x-auto prose-pre:rounded-lg prose-code:before:content-none prose-code:after:content-none prose-p:my-1.5 prose-headings:my-2 prose-ul:my-1.5 prose-ul:list-disc prose-ul:pl-5 prose-ol:my-1.5 prose-ol:list-decimal prose-ol:pl-5 prose-li:my-0.5 prose-li:marker:text-foreground/60 prose-hr:my-3 prose-hr:border-border'

const markdownComponents: Components = {
  pre({ node: _node, className, ...props }) {
    return <pre {...props} className={cn('scrollbar-thin', className)} />
  },
  ...untrustedMarkdownImage,
  ...untrustedMarkdownLink,
}

// ─── Sub-renderers ────────────────────────────────────────────────────────────

/**
 * `kind === 'question'`
 * Renders each question from AskUserQuestion with its options. If multiSelect
 * is true, uses checkboxes + a submit button instead of single-click options.
 * A free-text field is offered for write-in answers.
 */
function QuestionVariant({
  input,
  onResolve,
}: {
  input: unknown
  onResolve: (decision: PermissionDecision) => void
}) {
  const data = input as Partial<AskUserQuestionInput>
  const questions: AskUserQuestionItem[] = Array.isArray(data?.questions)
    ? (data.questions as AskUserQuestionItem[])
    : []

  // Per-question state: selected option label(s) and free-text.
  const [selections, setSelections] = useState<Record<string, string[]>>(() =>
    Object.fromEntries(questions.map((q) => [q.question, []]))
  )
  const [freeTexts, setFreeTexts] = useState<Record<string, string>>(() =>
    Object.fromEntries(questions.map((q) => [q.question, '']))
  )

  if (questions.length === 0) {
    return (
      <div className="text-xs text-muted-foreground italic">
        Malformed question payload — no questions found.
      </div>
    )
  }

  // With a single question, clicking a single-select option submits
  // immediately (snappy one-click UX — nothing else to answer). With
  // multiple questions, clicking only records the selection; the user must
  // hit the shared Submit button once every question has an answer, so
  // picking an option on question 1 doesn't prematurely submit empty
  // answers for questions 2+.
  const handleSingleSelect = (questionText: string, optionLabel: string) => {
    if (questions.length === 1) {
      onResolve({
        type: 'answer_question',
        answers: { [questionText]: optionLabel },
      })
      return
    }
    setSelections((prev) => ({ ...prev, [questionText]: [optionLabel] }))
  }

  const handleMultiSubmit = () => {
    const answers: Record<string, string> = {}
    for (const q of questions) {
      const sel = selections[q.question] ?? []
      const ft = freeTexts[q.question] ?? ''
      answers[q.question] = sel.length > 0 ? sel.join(', ') : ft
    }
    onResolve({ type: 'answer_question', answers })
  }

  const anyMultiSelect = questions.some((q) => q.multiSelect)
  const needsSubmitButton = questions.length > 1 || anyMultiSelect
  const allAnswered = questions.every((q) => {
    const sel = selections[q.question] ?? []
    const ft = (freeTexts[q.question] ?? '').trim()
    return sel.length > 0 || ft.length > 0
  })

  return (
    <div className="space-y-4">
      {questions.map((q) => {
        const isSingle = !q.multiSelect
        return (
          <div key={q.question} className="space-y-2">
            {q.header && (
              <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                {q.header}
              </div>
            )}
            <div className="text-sm font-medium">{q.question}</div>
            {isSingle ? (
              <div className="flex flex-wrap gap-2">
                {q.options.map((opt) => {
                  const isSelected = (
                    selections[q.question] ?? []
                  ).includes(opt.label)
                  return (
                    <Button
                      key={opt.label}
                      type="button"
                      size="sm"
                      variant={isSelected ? 'default' : 'outline'}
                      className="h-7 px-3 text-xs"
                      title={opt.description}
                      onClick={() =>
                        handleSingleSelect(q.question, opt.label)
                      }
                    >
                      {opt.label}
                    </Button>
                  )
                })}
                {/* Free-text / write-in fallback */}
                <div className="mt-1 flex w-full gap-2">
                  <Textarea
                    value={freeTexts[q.question] ?? ''}
                    onChange={(e) =>
                      setFreeTexts((prev) => ({
                        ...prev,
                        [q.question]: e.target.value,
                      }))
                    }
                    placeholder="Or type a custom answer..."
                    rows={2}
                    className="resize-none text-xs"
                  />
                  <Button
                    type="button"
                    size="sm"
                    variant="secondary"
                    className="h-auto self-end whitespace-nowrap text-xs"
                    disabled={!(freeTexts[q.question] ?? '').trim()}
                    onClick={() =>
                      handleSingleSelect(
                        q.question,
                        (freeTexts[q.question] ?? '').trim()
                      )
                    }
                  >
                    Submit
                  </Button>
                </div>
              </div>
            ) : (
              <div className="space-y-1.5">
                {q.options.map((opt) => {
                  const checked = (selections[q.question] ?? []).includes(
                    opt.label
                  )
                  return (
                    <label
                      key={opt.label}
                      className="flex cursor-pointer items-center gap-2 text-sm"
                    >
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={(e) => {
                          setSelections((prev) => {
                            const cur = prev[q.question] ?? []
                            return {
                              ...prev,
                              [q.question]: e.target.checked
                                ? [...cur, opt.label]
                                : cur.filter((l) => l !== opt.label),
                            }
                          })
                        }}
                        className="h-3.5 w-3.5"
                      />
                      <span>{opt.label}</span>
                      {opt.description && (
                        <span className="text-xs text-muted-foreground">
                          — {opt.description}
                        </span>
                      )}
                    </label>
                  )
                })}
              </div>
            )}
          </div>
        )
      })}

      {needsSubmitButton && (
        <div className="mt-2 flex gap-2">
          <Button
            type="button"
            size="sm"
            className="h-7 gap-1 px-3 text-xs"
            disabled={!allAnswered}
            onClick={handleMultiSubmit}
          >
            <Check className="h-3.5 w-3.5" />
            Submit {questions.length > 1 ? 'answers' : 'selection'}
          </Button>
        </div>
      )}
    </div>
  )
}

/**
 * `kind === 'plan_approval'`
 * Renders `input.plan` as markdown. Approve sends `ApprovePlan`; Reject opens
 * an optional feedback field then sends `RejectPlan`.
 */
function PlanApprovalVariant({
  input,
  onResolve,
}: {
  input: unknown
  onResolve: (decision: PermissionDecision) => void
}) {
  const data = input as Partial<PlanApprovalInput>
  const plan = data?.plan ?? ''
  const [rejecting, setRejecting] = useState(false)
  const [feedback, setFeedback] = useState('')

  return (
    <div className="space-y-3">
      {plan ? (
        <div className={cn(proseClasses, 'max-h-72 overflow-auto')}>
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            rehypePlugins={[[rehypeHighlight, { detect: true, ignoreMissing: true }]]}
            components={markdownComponents}
          >
            {plan}
          </ReactMarkdown>
        </div>
      ) : (
        <div className="text-xs text-muted-foreground italic">
          No plan text provided.
        </div>
      )}

      {rejecting ? (
        <div className="space-y-2">
          <Textarea
            value={feedback}
            onChange={(e) => setFeedback(e.target.value)}
            placeholder="Optional: explain why you're rejecting this plan…"
            rows={3}
            className="resize-none text-xs"
            autoFocus
          />
          <div className="flex gap-2">
            <Button
              type="button"
              size="sm"
              variant="destructive"
              className="h-7 gap-1 px-3 text-xs"
              onClick={() =>
                onResolve({
                  type: 'reject_plan',
                  feedback: feedback.trim() || undefined,
                })
              }
            >
              <X className="h-3.5 w-3.5" />
              Confirm rejection
            </Button>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              className="h-7 px-3 text-xs"
              onClick={() => setRejecting(false)}
            >
              Cancel
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex gap-2">
          <Button
            type="button"
            size="sm"
            className="h-7 gap-1 px-3 text-xs"
            onClick={() => onResolve({ type: 'approve_plan' })}
          >
            <Check className="h-3.5 w-3.5" />
            Approve plan
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-7 gap-1 px-3 text-xs"
            onClick={() => setRejecting(true)}
          >
            <X className="h-3.5 w-3.5" />
            Reject
          </Button>
        </div>
      )}
    </div>
  )
}

/**
 * `kind === 'tool_approval'`
 * Renders `tool_name` + the `input` object's fields. A `command` field gets a
 * `<code>` block; other fields render as a key/value list. Allow sends
 * `AllowTool`; Deny opens an optional reason field.
 */
function ToolApprovalVariant({
  toolName,
  input,
  onResolve,
}: {
  toolName: string
  input: unknown
  onResolve: (decision: PermissionDecision) => void
}) {
  const [denying, setDenying] = useState(false)
  const [reason, setReason] = useState('')

  const fields =
    input !== null &&
    typeof input === 'object' &&
    !Array.isArray(input)
      ? (Object.entries(input as Record<string, unknown>) as [string, unknown][])
      : []

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] font-semibold">
          {toolName}
        </span>
        <span className="text-xs text-muted-foreground">
          is requesting permission to run
        </span>
      </div>

      {fields.length > 0 && (
        <div className="space-y-1 rounded-lg border bg-background px-2.5 py-2 text-xs">
          {fields.map(([key, value]) =>
            key === 'command' ? (
              <div key={key}>
                <div className="mb-0.5 font-medium text-muted-foreground">
                  {key}
                </div>
                <code className="block whitespace-pre-wrap break-all rounded bg-muted/60 px-2 py-1 font-mono text-[11px]">
                  {String(value)}
                </code>
              </div>
            ) : (
              <div key={key} className="flex gap-2">
                <span className="shrink-0 font-medium text-muted-foreground">
                  {key}:
                </span>
                <span className="min-w-0 break-all font-mono text-[11px]">
                  {typeof value === 'object'
                    ? JSON.stringify(value)
                    : String(value ?? '')}
                </span>
              </div>
            )
          )}
        </div>
      )}

      {denying ? (
        <div className="space-y-2">
          <Textarea
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="Optional: reason for denying…"
            rows={2}
            className="resize-none text-xs"
            autoFocus
          />
          <div className="flex gap-2">
            <Button
              type="button"
              size="sm"
              variant="destructive"
              className="h-7 gap-1 px-3 text-xs"
              onClick={() =>
                onResolve({
                  type: 'deny_tool',
                  reason: reason.trim() || undefined,
                })
              }
            >
              <X className="h-3.5 w-3.5" />
              Confirm deny
            </Button>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              className="h-7 px-3 text-xs"
              onClick={() => setDenying(false)}
            >
              Cancel
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex gap-2">
          <Button
            type="button"
            size="sm"
            className="h-7 gap-1 px-3 text-xs"
            onClick={() => onResolve({ type: 'allow_tool' })}
          >
            <Check className="h-3.5 w-3.5" />
            Allow
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-7 gap-1 px-3 text-xs"
            onClick={() => setDenying(true)}
          >
            <X className="h-3.5 w-3.5" />
            Deny
          </Button>
        </div>
      )}
    </div>
  )
}

// ─── Settled state ────────────────────────────────────────────────────────────

function resolvedSummary(decision: PermissionDecision): string {
  switch (decision.type) {
    case 'allow_tool':
      return 'You allowed this tool.'
    case 'deny_tool':
      return decision.reason
        ? `You denied this tool — ${decision.reason}`
        : 'You denied this tool.'
    case 'answer_question': {
      const pairs = Object.entries(decision.answers)
        .map(([q, a]) => `${q}: ${a}`)
        .join('; ')
      return `You answered: ${pairs}`
    }
    case 'approve_plan':
      return 'You approved this plan.'
    case 'reject_plan':
      return decision.feedback
        ? `You rejected this plan — ${decision.feedback}`
        : 'You rejected this plan.'
  }
}

// ─── Public component ─────────────────────────────────────────────────────────

export interface PermissionCardProps {
  projectId: number
  /** Public ID of the conversation this permission belongs to. */
  conversationPublicId: string
  permission: PermissionRequest
  /**
   * Called after the decision is accepted by the server (204). The model's
   * reply normally arrives over the SSE connection that was already open when
   * the question was asked — but that connection can die while the user is
   * away deciding (tab backgrounded, network blip, dev server restart), in
   * which case the answer is still delivered to the model and persisted, but
   * the browser never sees it until a manual reload. The parent uses this to
   * poll for the reply so it shows up without one.
   */
  onResolved?: () => void
}

/**
 * Interactive card for a single pending permission request from the Claude CLI
 * bridge. Once resolved, shows a settled summary. On error, shows an inline
 * error and remains interactive so the user can retry.
 */
export function PermissionCard({
  projectId,
  conversationPublicId,
  permission,
  onResolved,
}: PermissionCardProps) {
  const [busy, setBusy] = useState(false)
  const [resolvedDecision, setResolvedDecision] =
    useState<PermissionDecision | null>(null)
  const [resolveError, setResolveError] = useState<string | null>(null)

  const handleResolve = async (decision: PermissionDecision) => {
    setBusy(true)
    setResolveError(null)
    try {
      await resolvePermission(
        projectId,
        conversationPublicId,
        permission.id,
        decision
      )
      setResolvedDecision(decision)
      onResolved?.()
    } catch (e) {
      setResolveError(
        e instanceof Error ? e.message : 'Could not resolve — please try again.'
      )
    } finally {
      setBusy(false)
    }
  }

  const kindIcon =
    permission.kind === 'question'
      ? HelpCircle
      : permission.kind === 'plan_approval'
        ? MapPin
        : Shield

  const KindIcon = kindIcon

  const kindLabel =
    permission.kind === 'question'
      ? 'Question'
      : permission.kind === 'plan_approval'
        ? 'Plan approval'
        : 'Tool approval'

  const borderColor =
    permission.kind === 'question'
      ? 'border-blue-500/30 bg-blue-500/5'
      : permission.kind === 'plan_approval'
        ? 'border-violet-500/30 bg-violet-500/5'
        : 'border-amber-500/30 bg-amber-500/5'

  const iconColor =
    permission.kind === 'question'
      ? 'text-blue-600 dark:text-blue-400'
      : permission.kind === 'plan_approval'
        ? 'text-violet-600 dark:text-violet-400'
        : 'text-amber-600 dark:text-amber-400'

  return (
    <div
      className={cn(
        'min-w-0 overflow-hidden rounded-lg border text-xs',
        borderColor
      )}
    >
      {/* Header */}
      <div className={cn('flex items-center gap-2 px-2.5 py-1.5 border-b', borderColor)}>
        <KindIcon className={cn('h-3.5 w-3.5 shrink-0', iconColor)} />
        <span className={cn('text-[11px] font-semibold uppercase tracking-wide', iconColor)}>
          {kindLabel}
        </span>
        {permission.kind !== 'question' && (
          <span className="font-mono text-[11px] text-muted-foreground">
            {permission.tool_name}
          </span>
        )}
      </div>

      {/* Body */}
      <div className="px-2.5 py-2">
        {resolvedDecision !== null ? (
          /* Settled state — non-interactive */
          <div className="flex items-start gap-2 text-muted-foreground">
            <Check className="mt-0.5 h-3.5 w-3.5 shrink-0 text-green-600 dark:text-green-400" />
            <span className="text-xs">{resolvedSummary(resolvedDecision)}</span>
          </div>
        ) : busy ? (
          <div className="flex items-center gap-2 text-muted-foreground">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            <span>Sending…</span>
          </div>
        ) : (
          <>
            {permission.kind === 'question' && (
              <QuestionVariant
                input={permission.input}
                onResolve={(d) => void handleResolve(d)}
              />
            )}
            {permission.kind === 'plan_approval' && (
              <PlanApprovalVariant
                input={permission.input}
                onResolve={(d) => void handleResolve(d)}
              />
            )}
            {permission.kind === 'tool_approval' && (
              <ToolApprovalVariant
                toolName={permission.tool_name}
                input={permission.input}
                onResolve={(d) => void handleResolve(d)}
              />
            )}
          </>
        )}

        {resolveError && (
          <div className="mt-2 flex items-start gap-1.5 text-xs text-destructive">
            <X className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            <span>{resolveError}</span>
          </div>
        )}
      </div>
    </div>
  )
}
