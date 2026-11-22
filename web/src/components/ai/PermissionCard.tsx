// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HighlightedCode } from '@/components/ui/code-block'
import { MarkdownCodeBlock } from '@/components/ui/markdown-code-block'

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
  Lock,
  MapPin,
  Send,
  Shield,
  X,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import {
  untrustedMarkdownImage,
  untrustedMarkdownLink,
} from '@/components/markdown/untrusted'
import {
  describeParameters,
  summaryTitle,
  type ParameterRow,
} from './platform-write-view'
import {
  GeneratedServiceLinkProposal,
  GeneratedServiceProposal,
  serviceLinkProposalViewModel,
  serviceProposalViewModel,
} from '@/components/ai/GeneratedServiceProposal'
import {
  resolvePermission as resolveProjectPermission,
  resolveUserPermission,
  type PermissionDecision,
} from '@/api/client'
import {
  chooseOption,
  chooseOther,
  composeAnswer,
  EMPTY_DRAFT,
  isAnswered,
  type QuestionDraft,
} from './question-answers'

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
// ─── Prose classes (mirrors DebugChatPanel) ───────────────────────────────────

const proseClasses =
  'prose prose-sm dark:prose-invert max-w-none prose-pre:bg-[#0d1117] prose-pre:text-xs prose-pre:border-0 prose-pre:overflow-x-auto prose-pre:rounded-lg prose-code:before:content-none prose-code:after:content-none prose-p:my-1.5 prose-headings:my-2 prose-ul:my-1.5 prose-ul:list-disc prose-ul:pl-5 prose-ol:my-1.5 prose-ol:list-decimal prose-ol:pl-5 prose-li:my-0.5 prose-li:marker:text-foreground/60 prose-hr:my-3 prose-hr:border-border'

const markdownComponents: Components = {
  pre: MarkdownCodeBlock,
  ...untrustedMarkdownImage,
  ...untrustedMarkdownLink,
}

// ─── Sub-renderers ────────────────────────────────────────────────────────────

/**
 * `kind === 'question'`
 * Renders each AskUserQuestion item as a vertical list of choices, with the
 * option descriptions visible, followed by an "Other" choice that opens a
 * text box for a written answer. Multi-select questions toggle; single-select
 * questions behave like radios. One Send button answers every question.
 */
function QuestionVariant({
  input,
  onResolve,
}: {
  input: unknown
  onResolve: (decision: PermissionDecision) => void
}) {
  const questions = questionItems(input)

  // Keyed by index: two questions may share their text.
  const [drafts, setDrafts] = useState<QuestionDraft[]>(() =>
    questions.map(() => EMPTY_DRAFT)
  )

  if (questions.length === 0) {
    return (
      <div className="text-xs text-muted-foreground italic">
        Malformed question payload — no questions found.
      </div>
    )
  }

  const update = (
    index: number,
    next: (draft: QuestionDraft) => QuestionDraft
  ) =>
    setDrafts((prev) =>
      prev.map((draft, i) => (i === index ? next(draft) : draft))
    )

  const answeredCount = drafts.filter(isAnswered).length
  const allAnswered = answeredCount === questions.length

  const submit = () => {
    if (!allAnswered) return
    const answers: Record<string, string> = {}
    questions.forEach((q, i) => {
      answers[q.question] = composeAnswer(drafts[i] ?? EMPTY_DRAFT)
    })
    onResolve({ type: 'answer_question', answers })
  }

  return (
    <div>
      <div className="divide-y divide-border/60">
        {questions.map((q, index) => (
          <QuestionItem
            key={index}
            item={q}
            position={questions.length > 1 ? index + 1 : null}
            total={questions.length}
            draft={drafts[index] ?? EMPTY_DRAFT}
            onChange={(next) => update(index, next)}
            onSubmit={submit}
          />
        ))}
      </div>

      <div className="flex items-center justify-between gap-3 border-t border-border/60 pt-3">
        <span className="text-xs text-muted-foreground">
          {questions.length > 1
            ? `${answeredCount} of ${questions.length} answered`
            : allAnswered
              ? 'Ready to send'
              : 'Choose an answer'}
        </span>
        <Button
          type="button"
          size="sm"
          className="h-8 gap-1.5 px-3"
          disabled={!allAnswered}
          onClick={submit}
        >
          <Send className="h-3.5 w-3.5" />
          {questions.length > 1 ? 'Send answers' : 'Send answer'}
        </Button>
      </div>
    </div>
  )
}

function questionItems(input: unknown): AskUserQuestionItem[] {
  const data = input as Partial<AskUserQuestionInput> | null
  return Array.isArray(data?.questions)
    ? (data.questions as AskUserQuestionItem[]).filter(
        (q) => q && typeof q.question === 'string'
      )
    : []
}

function QuestionItem({
  item,
  position,
  total,
  draft,
  onChange,
  onSubmit,
}: {
  item: AskUserQuestionItem
  /** 1-based position when there is more than one question. */
  position: number | null
  total: number
  draft: QuestionDraft
  onChange: (next: (draft: QuestionDraft) => QuestionDraft) => void
  onSubmit: () => void
}) {
  const multiSelect = item.multiSelect === true
  const options = Array.isArray(item.options) ? item.options : []

  return (
    <div className="space-y-2.5 py-3 first:pt-0">
      <div className="space-y-1">
        <div className="flex items-center gap-2 text-[11px] text-muted-foreground">
          {position !== null && (
            <span className="tabular-nums">
              {position}/{total}
            </span>
          )}
          {item.header && (
            <span className="rounded bg-muted px-1.5 py-0.5 font-medium text-foreground/80">
              {item.header}
            </span>
          )}
          {multiSelect && <span>Choose any</span>}
        </div>
        <p className="text-sm font-medium leading-snug text-foreground">
          {item.question}
        </p>
      </div>

      <div
        role={multiSelect ? 'group' : 'radiogroup'}
        aria-label={item.question}
        className="space-y-1.5"
      >
        {options.map((opt) => (
          <ChoiceRow
            key={opt.label}
            label={opt.label}
            description={opt.description}
            multiSelect={multiSelect}
            checked={draft.selected.includes(opt.label)}
            onSelect={() =>
              onChange((d) => chooseOption(d, opt.label, multiSelect))
            }
          />
        ))}
        <ChoiceRow
          label="Other"
          multiSelect={multiSelect}
          checked={draft.otherChosen}
          onSelect={() => onChange((d) => chooseOther(d, multiSelect))}
        />
        {draft.otherChosen && (
          <Textarea
            autoFocus
            value={draft.otherText}
            onChange={(e) => {
              const otherText = e.target.value
              onChange((d) => ({ ...d, otherText }))
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault()
                onSubmit()
              }
            }}
            aria-label={`Your answer to: ${item.question}`}
            placeholder="Type your answer…"
            rows={3}
            className="min-h-[72px] resize-y bg-background text-sm"
          />
        )}
      </div>
    </div>
  )
}

function ChoiceRow({
  label,
  description,
  multiSelect,
  checked,
  onSelect,
}: {
  label: string
  description?: string
  multiSelect: boolean
  checked: boolean
  onSelect: () => void
}) {
  return (
    <button
      type="button"
      role={multiSelect ? 'checkbox' : 'radio'}
      aria-checked={checked}
      onClick={onSelect}
      className={cn(
        'flex w-full items-start gap-3 rounded-md border bg-background px-3 py-2 text-left transition-colors',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
        checked
          ? 'border-primary/60 bg-primary/[0.04]'
          : 'border-border hover:border-foreground/25 hover:bg-muted/40'
      )}
    >
      <span
        aria-hidden
        className={cn(
          'mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center border transition-colors',
          multiSelect ? 'rounded-[4px]' : 'rounded-full',
          checked
            ? 'border-primary bg-primary text-primary-foreground'
            : 'border-muted-foreground/40'
        )}
      >
        {checked &&
          (multiSelect ? (
            <Check className="h-3 w-3" strokeWidth={3} />
          ) : (
            <span className="h-1.5 w-1.5 rounded-full bg-primary-foreground" />
          ))}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm leading-5 text-foreground">{label}</span>
        {description && (
          <span className="mt-0.5 block text-xs leading-snug text-muted-foreground">
            {description}
          </span>
        )}
      </span>
    </button>
  )
}

/** The settled view of an answered question card: each question and its answer. */
function AnsweredQuestions({ answers }: { answers: Record<string, unknown> }) {
  const entries = Object.entries(answers)
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Check className="h-3.5 w-3.5 text-green-600 dark:text-green-400" />
        You answered
      </div>
      <dl className="space-y-1.5">
        {entries.map(([question, answer]) => (
          <div key={question} className="text-xs">
            <dt className="text-muted-foreground">{question}</dt>
            <dd className="font-medium text-foreground">{String(answer)}</dd>
          </div>
        ))}
      </dl>
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
        <div
          tabIndex={0}
          className={cn(proseClasses, 'max-h-72 overflow-auto')}
        >
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
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
/** What a platform change will do, as a titled list instead of raw JSON. */
function PlatformWriteSummary({
  title,
  operation,
  rows,
  carriesHandle,
}: {
  title: string | null
  operation: string | null
  rows: ParameterRow[]
  carriesHandle: boolean
}) {
  return (
    <div className="space-y-2 rounded-lg border bg-background px-3 py-2.5 text-xs">
      <div className="min-w-0">
        <p className="text-sm font-medium">{title ?? operation ?? 'Change'}</p>
        {operation && title && (
          <code className="text-[11px] text-muted-foreground">{operation}</code>
        )}
      </div>
      {rows.length > 0 && (
        <dl className="grid grid-cols-[minmax(6rem,max-content)_minmax(0,1fr)] gap-x-4 gap-y-1">
          {rows.map((row) => (
            <div key={row.key} className="contents">
              <dt className="text-muted-foreground" title={row.key}>
                {row.label}
              </dt>
              <dd className="min-w-0 break-all font-mono text-[11px]">
                {row.value.kind === 'hidden' ? (
                  <span className="inline-flex items-center gap-1 font-sans text-muted-foreground">
                    <Lock className="size-3" />
                    Hidden, provided by you
                  </span>
                ) : (
                  row.value.text
                )}
              </dd>
            </div>
          ))}
        </dl>
      )}
      {carriesHandle && (
        <p className="flex items-start gap-1.5 border-t pt-2 text-muted-foreground">
          <Shield className="mt-0.5 size-3.5 shrink-0" />
          Uses the values you entered privately. Temps adds them when you
          approve; the assistant never saw them.
        </p>
      )}
    </div>
  )
}

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
  const isPlatformWrite =
    toolName === 'temps_write' || toolName === 'mcp__temps-chat__temps_write'

  const platformInput =
    isPlatformWrite &&
    input !== null &&
    typeof input === 'object' &&
    !Array.isArray(input)
      ? (input as Record<string, unknown>)
      : null
  const operation =
    typeof platformInput?.operation === 'string'
      ? platformInput.operation
      : null
  const parameters = platformInput?.parameters
  const serviceProposal =
    operation === 'create_service' &&
    parameters !== null &&
    typeof parameters === 'object'
      ? serviceProposalViewModel(JSON.stringify(parameters))
      : null
  const targetProjectId =
    parameters !== null &&
    typeof parameters === 'object' &&
    !Array.isArray(parameters) &&
    typeof (parameters as Record<string, unknown>).project_id === 'number'
      ? ((parameters as Record<string, unknown>).project_id as number)
      : typeof platformInput?.project_id === 'number'
        ? platformInput.project_id
        : null
  const serviceLinkProposal =
    operation === 'link_service_to_project'
      ? serviceLinkProposalViewModel(parameters)
      : null
  const hasRichPlatformCard =
    serviceProposal !== null || serviceLinkProposal !== null

  const plainPlatformWrite = platformInput !== null && !hasRichPlatformCard
  const writeTitle = summaryTitle(
    typeof platformInput?.summary === 'string' ? platformInput.summary : null
  )
  const parameterRows = describeParameters(parameters)

  const fields =
    input !== null && typeof input === 'object' && !Array.isArray(input)
      ? (Object.entries(input as Record<string, unknown>) as [
          string,
          unknown,
        ][])
      : []

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] font-semibold">
          {toolName}
        </span>
        <span className="text-xs text-muted-foreground">
          {isPlatformWrite
            ? 'will execute this platform change after approval'
            : 'is requesting permission to run'}
        </span>
      </div>

      {serviceProposal && (
        <div className="overflow-hidden rounded-lg border bg-background">
          <GeneratedServiceProposal
            proposal={serviceProposal}
            summary={
              typeof platformInput?.summary === 'string'
                ? platformInput.summary
                : undefined
            }
            statusLabel="Awaiting your approval"
            statusClassName="text-amber-700 dark:text-amber-400"
            projectId={targetProjectId}
          />
        </div>
      )}

      {serviceLinkProposal && (
        <div className="overflow-hidden rounded-lg border bg-background">
          <GeneratedServiceLinkProposal proposal={serviceLinkProposal} />
        </div>
      )}

      {plainPlatformWrite && (
        <PlatformWriteSummary
          title={writeTitle}
          operation={operation}
          rows={parameterRows}
          carriesHandle={platformInput?.carries_handle === true}
        />
      )}

      {fields.length > 0 && !hasRichPlatformCard && !plainPlatformWrite && (
        <div className="space-y-1 rounded-lg border bg-background px-2.5 py-2 text-xs">
          {fields.map(([key, value]) =>
            key === 'command' ? (
              <div key={key}>
                <div className="mb-0.5 font-medium text-muted-foreground">
                  {key}
                </div>
                <HighlightedCode
                  code={String(value)}
                  language="bash"
                  className="block whitespace-pre-wrap break-all rounded bg-muted/60 px-2 py-1 font-mono text-[11px]"
                />
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
            {isPlatformWrite ? 'Approve & run' : 'Allow'}
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

function answerMap(answers: unknown): Record<string, unknown> {
  return answers && typeof answers === 'object' && !Array.isArray(answers)
    ? (answers as Record<string, unknown>)
    : {}
}

function resolvedSummary(decision: PermissionDecision): string {
  switch (decision.type) {
    case 'allow_tool':
      return 'You allowed this tool.'
    case 'deny_tool':
      return decision.reason
        ? `You denied this tool — ${decision.reason}`
        : 'You denied this tool.'
    case 'answer_question': {
      const pairs = Object.entries(answerMap(decision.answers))
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
  /** Public ID of the conversation this permission belongs to. */
  conversationPublicId: string
  /** User-owned chats use the global endpoint; project chats require projectId. */
  userScoped: boolean
  projectId?: number
  permission: PermissionRequest
  /**
   * Called after the decision is accepted by the server (204). The model's
   * reply normally arrives over the SSE connection that was already open when
   * the question was asked — but that connection can die while the user is
   * away deciding (tab backgrounded, network blip, dev server restart). The
   * structured decision is still delivered through the provider callback and
   * the resulting assistant turn is persisted, but the browser may miss its
   * live events. The parent polls for that result so it appears without a
   * manual reload.
   */
  onResolved?: () => void
}

/**
 * Interactive card for a single pending permission request from the Claude CLI
 * bridge. Once resolved, shows a settled summary. On error, shows an inline
 * error and remains interactive so the user can retry.
 */
export function PermissionCard({
  conversationPublicId,
  permission,
  userScoped,
  projectId,
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
      if (userScoped) {
        await resolveUserPermission({
          body: { decision },
          path: {
            public_id: conversationPublicId,
            permission_id: permission.id,
          },
          throwOnError: true,
        })
      } else {
        if (projectId == null) {
          throw new Error('Service permission resolution requires a service.')
        }
        await resolveProjectPermission({
          body: { decision },
          path: {
            project_id: projectId,
            public_id: conversationPublicId,
            permission_id: permission.id,
          },
          throwOnError: true,
        })
      }
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

  const questionCount =
    permission.kind === 'question' ? questionItems(permission.input).length : 0

  const kindLabel =
    permission.kind === 'question'
      ? questionCount > 1
        ? `${questionCount} questions`
        : 'Question'
      : permission.kind === 'plan_approval'
        ? 'Plan approval'
        : 'Tool approval'

  const borderColor =
    permission.kind === 'question'
      ? 'border-blue-500/25 bg-card'
      : permission.kind === 'plan_approval'
        ? 'border-violet-500/30 bg-violet-500/5'
        : 'border-amber-500/30 bg-amber-500/5'

  const iconColor =
    permission.kind === 'question'
      ? 'text-blue-600 dark:text-blue-400'
      : permission.kind === 'plan_approval'
        ? 'text-violet-600 dark:text-violet-400'
        : 'text-amber-700 dark:text-amber-400'

  return (
    <div
      className={cn(
        'min-w-0 overflow-hidden rounded-lg border text-xs',
        borderColor
      )}
    >
      {/* Header */}
      <div
        className={cn(
          'flex items-center gap-2 px-2.5 py-1.5 border-b',
          permission.kind === 'question'
            ? 'border-blue-500/15 bg-blue-500/[0.06]'
            : borderColor
        )}
      >
        <KindIcon className={cn('h-3.5 w-3.5 shrink-0', iconColor)} />
        <span
          className={cn(
            'text-[11px] font-semibold uppercase tracking-wide',
            iconColor
          )}
        >
          {kindLabel}
        </span>
        {permission.kind !== 'question' && (
          <span className="font-mono text-[11px] text-muted-foreground">
            {permission.tool_name}
          </span>
        )}
      </div>

      {/* Body */}
      <div className={permission.kind === 'question' ? 'p-3' : 'px-2.5 py-2'}>
        {resolvedDecision?.type === 'answer_question' ? (
          <AnsweredQuestions answers={answerMap(resolvedDecision.answers)} />
        ) : resolvedDecision !== null ? (
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
