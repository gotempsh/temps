// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState, type ReactNode } from 'react'
import {
  BarChart3, Bot, Brain, Bug, ChevronDown, ChevronRight, Container, Database, FilePen, FileText,
  Gauge, GitBranch, Globe, HelpCircle, KeyRound, ListChecks, ListOrdered, Rocket, ScrollText,
  Search, Terminal, type LucideIcon,
} from 'lucide-react'
import { Button } from './ui/button'
import { cn } from './lib/cn'
import { Kbd } from './kbd'
import { EchoDialog } from './echo-dialog'
import { fmtNum } from './fmt'
import { GLYPH, GLYPH_CLASS, Status, type State } from './status'
import { KeyValue, type KV } from './templates'

/* ────────────────────────────────────────────────────────────────────────
   Agent primitives: what an AI renders, and what proves it.

   The rule these enforce (brand-guidelines §0, "AI-native, under policy"):
   an agent is an operator, so its work is a ledger of typed tool calls with
   state words, every block it generates carries the call that produced it,
   and every write is proposed, never performed. Nothing here can draw a
   chat bubble, an avatar, or a block without a source.

   See `design-system/docs/generative-ui.md`. The reference surface is
   `/agent`.
   ──────────────────────────────────────────────────────────────────────── */

// ── Vocabulary ─────────────────────────────────────────────────────────

/** The six states of a tool call, plus the two approval outcomes. */
export type ToolState =
  | 'input-streaming' | 'input-available' | 'output-available' | 'output-error'
  | 'approval-requested' | 'approval-responded' | 'output-denied'

/** State glyph and state word for each. The word is never invented at a call site. */
export const TOOL_STATE: Record<ToolState, { state: State; word: string }> = {
  'input-streaming': { state: 'idle', word: 'preparing' },
  'input-available': { state: 'warn', word: 'running' },
  'output-available': { state: 'ok', word: 'done' },
  'output-error': { state: 'error', word: 'failed' },
  'approval-requested': { state: 'warn', word: 'needs approval' },
  'approval-responded': { state: 'ok', word: 'approved' },
  'output-denied': { state: 'idle', word: 'denied' },
}

/** What the call IS. One concept, one icon — the table in `docs/icons.md`. */
export type ToolKind =
  | 'command' | 'edit' | 'read' | 'search' | 'fetch' | 'git' | 'subagent' | 'reasoning'
  | 'plan' | 'tasks' | 'question' | 'query' | 'metrics' | 'errors' | 'logs' | 'deploy'
  | 'database' | 'config' | 'service'

const KIND_ICON: Record<ToolKind, LucideIcon> = {
  command: Terminal, edit: FilePen, read: FileText, search: Search, fetch: Globe, git: GitBranch,
  subagent: Bot, reasoning: Brain, plan: ListOrdered, tasks: ListChecks, question: HelpCircle,
  query: BarChart3, metrics: Gauge, errors: Bug, logs: ScrollText, deploy: Rocket,
  database: Database, config: KeyRound, service: Container,
}

/**
 * Tool name → kind. The console's assistant calls two virtual tools, `temps`
 * (read) and `temps_write` (propose), each dispatching to one allowlisted API
 * operation — so the name a row shows is the operation, and that is what this
 * maps. Coding-agent tool names are here too. Unknown names read as a command,
 * which is the honest default.
 */
const TOOL_KIND: Record<string, ToolKind> = {
  // coding agent
  run_command: 'command', bash: 'command',
  edit_file: 'edit', write_file: 'edit', apply_patch: 'edit',
  read_file: 'read', grep: 'search', glob: 'search',
  fetch: 'fetch', web_search: 'fetch', git: 'git',
  // console assistant · reads
  query_traces: 'query', get_trace: 'query', get_visitor_stats: 'query', get_events_timeline: 'query',
  query_metrics: 'metrics', get_metrics_over_time: 'metrics', get_container_metrics: 'metrics',
  list_error_groups: 'errors', get_error_group: 'errors', get_error_time_series: 'errors',
  query_logs: 'logs', get_container_logs: 'logs', get_deployment_job_logs: 'logs',
  get_deployment: 'deploy', get_project_deployments: 'deploy', get_last_deployment: 'deploy',
  get_environment_variables: 'config', get_resolved_environment_variables: 'config',
  list_containers: 'service', get_container_info: 'service',
  read_entity_rows: 'database', list_entities: 'database',
  // console assistant · writes (proposed, never performed)
  rollback_to_deployment: 'deploy', promote_deployment: 'deploy', trigger_project_pipeline: 'deploy',
  restart_container: 'service', stop_container: 'service', start_container: 'service',
  create_environment_variable: 'config', update_environment_variable: 'config', delete_environment_variable: 'config',
}

/** The kind of a call, from its name and its first argument. */
export function toolKind(name: string, arg?: string): ToolKind {
  if ((name === 'run_command' || name === 'bash') && arg?.startsWith('git ')) return 'git'
  return TOOL_KIND[name] ?? 'command'
}

export function toolIcon(kind: ToolKind): LucideIcon {
  return KIND_ICON[kind]
}

// ── The pieces a row is made of ────────────────────────────────────────

/** The state glyph, in its own fixed slot. Never shares the kind slot. */
export function AgentGlyph({ state, className }: { state: State; className?: string }) {
  return <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[state], className)}>{GLYPH[state]}</span>
}

/**
 * The kind icon: what the thing IS. Muted by default; only failure and a
 * waiting approval tint it, because those are the two the reader must find
 * by scanning (handoff §7b).
 */
export function AgentKindIcon({ icon: I, state, className }: { icon: LucideIcon; state: State; className?: string }) {
  return <I aria-hidden className={cn('h-3.5 w-3.5 shrink-0', state === 'error' ? 'text-destructive' : state === 'warn' ? 'text-warning' : 'text-muted-foreground', className)} />
}

/**
 * One line of the ledger: kind icon · title · meta · disclosure. No frame —
 * inside a turn there are no boxes. Children hang under it, indented.
 */
export function AgentRow({ icon, state, title, meta, open, onToggle, children, className, accent }: {
  icon: LucideIcon
  state: State
  title: ReactNode
  meta?: ReactNode
  open: boolean
  onToggle: () => void
  children?: ReactNode
  className?: string
  /** A red left rule. Irreversible approvals only. */
  accent?: 'destructive'
}) {
  return (
    <div className={cn(accent === 'destructive' && 'border-s-2 border-destructive ps-2', className)}>
      <button type="button" onClick={onToggle} aria-expanded={open} className="group -mx-1 flex min-h-7 w-[calc(100%+0.5rem)] items-center gap-2 px-1 py-1 text-start text-xs hover:bg-muted">
        <AgentKindIcon icon={icon} state={state} />
        <span className="min-w-0 flex-1 truncate [&>.break-all]:whitespace-normal">{title}</span>
        {meta && <span className={cn('shrink-0 font-mono text-[11px]', state === 'error' ? 'text-destructive' : state === 'warn' ? 'text-warning' : 'text-muted-foreground')}>{meta}</span>}
        {children !== undefined && (open ? <ChevronDown className="h-3 w-3 shrink-0 opacity-40 group-hover:opacity-70" /> : <ChevronRight className="h-3 w-3 shrink-0 opacity-40 group-hover:opacity-70" />)}
      </button>
      {open && <div className="ps-5">{children}</div>}
    </div>
  )
}

/** An inset pane: a tool's input, its output, a query. Mono, no frame. */
export function AgentInset({ label, children, className }: { label?: string; children: ReactNode; className?: string }) {
  return (
    <div className={cn('op-inset my-1 px-2 py-1.5 font-mono text-[11px] leading-5', className)}>
      {label && <p className="op-label mb-1 text-[9px]">{label}</p>}
      <pre className="overflow-x-auto whitespace-pre-wrap break-words">{children}</pre>
    </div>
  )
}

/**
 * A unified diff, line by line. Ink for what is there now, muted and struck
 * for what went; the sign is the only colour on the line.
 */
export function AgentDiff({ text }: { text: string }) {
  return (
    <pre className="op-inset my-1 overflow-x-auto px-2 py-1.5 font-mono text-[11px] leading-5">
      {text.split('\n').map((l, i) => {
        const k = l.startsWith('+') ? 'add' : l.startsWith('-') ? 'del' : l.startsWith('@@') ? 'hunk' : 'ctx'
        return (
          <div key={i} className={cn('flex gap-2', k === 'del' && 'text-muted-foreground line-through decoration-[var(--op-rule-soft)]', k === 'hunk' && 'text-muted-foreground', k === 'ctx' && 'text-muted-foreground')}>
            <span aria-hidden className={cn('w-3 shrink-0 select-none text-center no-underline', k === 'add' && 'text-success', k === 'del' && 'text-destructive')}>{k === 'add' ? '+' : k === 'del' ? '−' : k === 'hunk' ? '@' : ' '}</span>
            <span className="min-w-0 whitespace-pre-wrap break-words">{l.replace(/^[-+]/, '')}</span>
          </div>
        )
      })}
    </pre>
  )
}

// ── ToolRow ────────────────────────────────────────────────────────────

export type ToolApproval = {
  /** What it does, to whom, and how to undo it. Ends in "cannot be undone" when it cannot. */
  reason: string
  /** Irreversible loss only. Red left rule, red confirm, no "always". */
  destructive?: boolean
  onRespond: (r: 'once' | 'session' | 'deny') => void
}

/**
 * One typed tool call as a row of the ledger: kind icon · name · argument ·
 * state word · duration, opening to its input, its output, its diff or its
 * error. A failing call is × and its error sentence — never hidden, never
 * collapsed away.
 *
 * Edits and commands open by default because the diff and the output ARE the
 * content; reads, searches and queries collapse.
 */
export function ToolRow({ name, arg, kind, state, ms, input, output, diff, error, meta, defaultOpen, approval, approved, children }: {
  name: string
  arg?: string
  /** Override the kind derived from `name`. */
  kind?: ToolKind
  state: ToolState
  /** Duration in ms. Rendered beside "done"; pass the live elapsed while running. */
  ms?: number
  input?: string
  output?: ReactNode
  diff?: string
  error?: string
  /** Replaces the state word on the right. Use only when the row is not a call. */
  meta?: ReactNode
  defaultOpen?: boolean
  approval?: ToolApproval
  /** The call needed approval and got it, so the record says so beside the timing. */
  approved?: boolean
  /** Generated blocks the call produced, rendered under the row. */
  children?: ReactNode
}) {
  const k = kind ?? toolKind(name, arg)
  const isCmd = k === 'command' || k === 'git'
  const [open, setOpen] = useState(defaultOpen ?? (!!diff || isCmd || state === 'approval-requested' || state === 'output-error'))
  const s = TOOL_STATE[state]
  const dur = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}s` : `${n}ms`)
  // The badges promise Y / N; bind them while this approval is the pending one.
  // Typing in the composer is left alone.
  useEffect(() => {
    if (state !== 'approval-requested' || !approval) return
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || e.metaKey || e.ctrlKey || e.altKey) return
      if (e.key === 'y' || e.key === 'Y') { e.preventDefault(); approval.onRespond('once') }
      else if (e.key === 'n' || e.key === 'N') { e.preventDefault(); approval.onRespond('deny') }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [state, approval])
  return (
    <AgentRow icon={toolIcon(k)} state={s.state} open={open} onToggle={() => setOpen((o) => !o)}
      title={<span className={cn('font-mono', isCmd && 'whitespace-normal break-all')}><span className="font-medium">{isCmd ? '$' : name}</span>{arg && <span className={cn(isCmd ? 'text-foreground' : 'text-muted-foreground')}> {arg}</span>}</span>}
      meta={meta ?? <>{approved && state === 'output-available' ? 'approved · ' : ''}{s.word}{ms !== undefined && (state === 'output-available' || state === 'input-available') && ` · ${dur(ms)}`}{state === 'input-streaming' && <span className="op-caret" />}</>}
      accent={approval?.destructive && state === 'approval-requested' ? 'destructive' : undefined}>
      {input && !isCmd && <AgentInset label="input">{input}</AgentInset>}
      {diff && state === 'output-available' && <AgentDiff text={diff} />}
      {state === 'approval-requested' && approval && (
        <div className="space-y-2 py-1 text-xs">
          <p>{approval.reason}</p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" className={cn('h-7 text-xs', approval.destructive ? 'op-fill-destructive' : 'op-primary')} onClick={() => approval.onRespond('once')}>{approval.destructive ? 'run it' : 'approve'} <Kbd keys="Y" className="ms-1 opacity-70" /></Button>
            {!approval.destructive && <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => approval.onRespond('session')}>always for this session</Button>}
            <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => approval.onRespond('deny')}>deny <Kbd keys="N" className="ms-1 opacity-70" /></Button>
            <span className="ms-auto self-center text-[11px] text-muted-foreground">the agent waits; nothing runs until you answer</span>
          </div>
        </div>
      )}
      {state === 'output-denied' && <div className="py-1 text-xs text-muted-foreground">denied · the agent was told why and will try another way</div>}
      {output !== undefined && state === 'output-available' && !diff && <AgentInset label={isCmd ? undefined : 'output'}>{output}</AgentInset>}
      {error && state === 'output-error' && <AgentInset label="error" className="text-destructive">{error}</AgentInset>}
      {children}
    </AgentRow>
  )
}

// ── Provenance ─────────────────────────────────────────────────────────

/**
 * The source line every generated block carries: `from query_metrics · 41m
 * ago · 7d`, with the query itself one click away. A block an agent drew
 * with no call behind it is not a block, so `tool` and `when` are required —
 * the component cannot be used to launder an unsourced picture.
 */
export function Provenance({ tool, when, range, note, query, queryLabel = 'query', children, className }: {
  /** The tool call that produced the block, verbatim. */
  tool: string
  /** When it ran, relative under a day (`41m ago`). */
  when: string
  /** The window it read (`7d`, `24h`). */
  range?: string
  /** One more fact: sampling, retention, the row count. */
  note?: ReactNode
  /** What was actually sent. Shown under a "show query" toggle. */
  query?: string
  queryLabel?: string
  children: ReactNode
  className?: string
}) {
  const [open, setOpen] = useState(false)
  return (
    <div className={cn('min-w-0 space-y-1', className)}>
      <div className="min-w-0">{children}</div>
      <p className="flex flex-wrap items-baseline gap-x-2 gap-y-1 font-mono text-[11px] text-muted-foreground">
        <span>from {tool}</span>
        <span aria-hidden>·</span><span>{when}</span>
        {range && <><span aria-hidden>·</span><span>{range}</span></>}
        {note && <><span aria-hidden>·</span><span className="min-w-0">{note}</span></>}
        {query && (
          <button type="button" aria-expanded={open} onClick={() => setOpen((o) => !o)} className="ms-auto underline decoration-[var(--op-rule-soft)] underline-offset-4 hover:text-foreground">
            {open ? 'hide query' : 'show query'}
          </button>
        )}
      </p>
      {open && query && <AgentInset label={queryLabel}>{query}</AgentInset>}
    </div>
  )
}

// ── Proposal ───────────────────────────────────────────────────────────

/**
 * A write the agent wants to make, before it makes it. Four facts, always,
 * in this order: the action, the target, the consequence, and whether it can
 * be undone. The reader confirms or declines; the agent never confirms its
 * own proposal.
 *
 * `irreversible` routes the confirm through `EchoDialog` (typed echo, red),
 * because red means loss nobody can get back — a reversible deploy asks in
 * ink.
 */
export function Proposal({ action, target, consequence, reversal, irreversible, autonomy, kind = 'deploy', confirmWord, confirmLabel, declineLabel = 'decline', steps, decided, onConfirm, onDecline, className }: {
  /** Verb first: "roll back", "set the variable", "drop the database". */
  action: string
  /** What it acts on, as an identifier. */
  target: string
  /** What changes for whom, in one sentence. */
  consequence: string
  /** How it is undone, or why it cannot be. */
  reversal: string
  /** True only when the loss is permanent. */
  irreversible?: boolean
  /** The autonomy level this capability runs at, in words. Shown, never assumed. */
  autonomy?: string
  kind?: ToolKind
  /** The word typed into `EchoDialog`. Defaults to `target`. */
  confirmWord?: string
  confirmLabel?: string
  declineLabel?: string
  /** The steps the backend runs, ticked in the dialog. */
  steps?: string[]
  decided?: 'confirmed' | 'declined' | null
  onConfirm: () => void
  onDecline: () => void
  className?: string
}) {
  const pending = !decided
  const rows: KV[] = [
    { k: 'action', v: action, mono: false },
    { k: 'target', v: target },
    { k: 'consequence', v: consequence, mono: false },
    { k: 'reversible', v: reversal, mono: false, state: irreversible ? 'error' : 'ok' },
    ...(autonomy ? [{ k: 'autonomy', v: autonomy, mono: false }] : []),
  ]
  return (
    <div className={cn('border', pending && 'op-raise', className)}>
      <p className="flex items-center gap-2 border-b px-2 py-1.5 text-xs">
        <AgentKindIcon icon={toolIcon(kind)} state={pending ? 'warn' : decided === 'confirmed' ? 'ok' : 'idle'} />
        <span className="font-medium">proposal</span>
        <span className="min-w-0 truncate font-mono text-muted-foreground">{action} · {target}</span>
        <span className="ms-auto shrink-0 font-mono text-[11px] text-muted-foreground">{pending ? 'waiting for you' : decided}</span>
      </p>
      <KeyValue rows={rows} compact />
      <div className="flex flex-wrap items-center gap-2 border-t px-2 py-1.5 text-[11px] text-muted-foreground">
        {pending ? (
          <>
            {irreversible ? (
              <EchoDialog
                destructive
                trigger={<Button size="sm" className="op-fill-destructive h-7 text-xs">{confirmLabel ?? action}</Button>}
                title={`${action} ${target}`}
                description={`${consequence} ${reversal} Type the name to confirm.`}
                confirmWord={confirmWord ?? target}
                steps={steps ?? [action]}
                onDone={onConfirm}
              />
            ) : (
              <Button size="sm" className="op-primary h-7 text-xs" onClick={onConfirm}>{confirmLabel ?? action}</Button>
            )}
            <Button size="sm" variant="outline" className="h-7 text-xs" onClick={onDecline}>{declineLabel}</Button>
            <span className="ms-auto">nothing has run · the agent waits for you</span>
          </>
        ) : decided === 'confirmed' ? (
          <Status state="ok" label={`confirmed · you approved this, the agent ran it`} className="font-mono" />
        ) : (
          <Status state="idle" label="declined · nothing ran" className="font-mono" />
        )}
      </div>
    </div>
  )
}

// ── Streaming ──────────────────────────────────────────────────────────

/** Which block the skeleton stands in for. It must match what lands. */
export type StreamKind = 'text' | 'chart' | 'ledger' | 'detail' | 'keyvalue' | 'tool'

const BAR = 'h-2 bg-muted'

/**
 * The shape of the block that is coming, held open while it streams, so the
 * page does not jump when it lands. Static: a skeleton that shimmers is
 * banned, and a chart never draws itself.
 */
export function StreamBlock({ kind, label, className }: { kind: StreamKind; label?: string; className?: string }) {
  return (
    <div role="status" aria-live="polite" aria-busy className={cn('min-w-0 space-y-2', className)}>
      {kind === 'text' && (
        <div className="max-w-[68ch] space-y-1.5 text-sm">
          <div className={cn(BAR, 'w-full')} />
          <div className={cn(BAR, 'w-11/12')} />
          <div className="flex items-center"><div className={cn(BAR, 'w-5/12')} /><span className="op-caret" aria-hidden /></div>
        </div>
      )}
      {kind === 'chart' && (
        <div className="border p-2">
          <div className="flex h-[140px] items-end gap-1" aria-hidden>
            {[38, 52, 44, 61, 57, 70, 48, 66, 59, 74, 63, 51].map((h, i) => <div key={i} className="flex-1 bg-muted" style={{ height: `${h}%` }} />)}
          </div>
        </div>
      )}
      {kind === 'ledger' && (
        <div className="border">
          {[0, 1, 2, 3, 4].map((i) => (
            <div key={i} className="flex items-center gap-3 border-b border-[var(--op-rule-soft)] px-3 py-2 last:border-b-0">
              <div className={cn(BAR, 'w-3')} /><div className={cn(BAR, 'w-2/5')} /><div className={cn(BAR, 'ms-auto w-16')} />
            </div>
          ))}
        </div>
      )}
      {kind === 'detail' && (
        <div className="space-y-2">
          <div className={cn(BAR, 'h-3 w-56')} />
          <div className="border p-3 space-y-1.5">{[0, 1, 2, 3].map((i) => <div key={i} className="flex gap-4"><div className={cn(BAR, 'w-24')} /><div className={cn(BAR, 'w-1/3')} /></div>)}</div>
        </div>
      )}
      {kind === 'keyvalue' && (
        <div className="space-y-1.5">{[0, 1, 2, 3].map((i) => <div key={i} className="flex gap-4"><div className={cn(BAR, 'w-20')} /><div className={cn(BAR, 'w-2/5')} /></div>)}</div>
      )}
      {kind === 'tool' && (
        <div className="flex min-h-7 items-center gap-2"><div className={cn(BAR, 'w-3.5')} /><div className={cn(BAR, 'w-1/3')} /><div className={cn(BAR, 'ms-auto w-14')} /></div>
      )}
      <p className="font-mono text-[11px] text-muted-foreground">{label ?? `drawing the ${kind}`}<span className="op-caret" aria-hidden /></p>
    </div>
  )
}

// ── Question ───────────────────────────────────────────────────────────

/**
 * The agent needs a decision, so it asks with typed options — never a
 * free-text "please provide". Answering is two steps: pick (radio, ○ → ●,
 * 1–4 from the keyboard), then confirm (⏎), because one click sending an
 * answer makes a misclick mid-run irreversible. The unanswered question is
 * the one raised element on the screen.
 */
export function AgentQuestion({ q, options, answer, onAnswer, hint = 'or type an answer below · the agent waits' }: {
  q: string
  options: { label: string; note: string }[]
  answer: string | null
  onAnswer: (a: string) => void
  hint?: ReactNode
}) {
  const [picked, setPicked] = useState<string | null>(null)
  const chosen = answer ?? picked
  useEffect(() => {
    if (answer) return
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || e.metaKey || e.ctrlKey) return
      const n = Number(e.key)
      if (n >= 1 && n <= options.length) setPicked(options[n - 1].label)
      else if (e.key === 'Enter' && picked) { e.preventDefault(); onAnswer(picked) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [answer, picked, options, onAnswer])
  return (
    <div className={cn('border', !answer && 'op-raise')} role="radiogroup" aria-label={q}>
      <p className="flex items-center gap-2 border-b px-2 py-1.5 text-xs">
        <AgentKindIcon icon={HelpCircle} state={answer ? 'ok' : 'warn'} />
        <span className="font-medium">{q}</span>
        {answer && <span className="ms-auto font-mono text-[11px] text-muted-foreground">answered</span>}
      </p>
      <div className="grid gap-px sm:grid-cols-2">
        {options.map((o, i) => (
          <button key={o.label} type="button" role="radio" aria-checked={chosen === o.label} disabled={!!answer} onClick={() => setPicked(o.label)}
            className={cn('flex items-start gap-2 px-2 py-2 text-start text-xs', !answer && 'hover:bg-muted', answer === o.label && 'op-fill-ink border-0', !answer && picked === o.label && 'bg-muted', answer && answer !== o.label && 'text-muted-foreground')}>
            <span aria-hidden className="mt-px w-3 text-center">{chosen === o.label ? '●' : '○'}</span>
            <span className="min-w-0 flex-1"><span className="block font-medium">{o.label}</span><span className={cn('block text-[11px]', answer === o.label ? 'text-background/70' : 'text-muted-foreground')}>{o.note}</span></span>
            {!answer && <Kbd keys={String(i + 1)} className="hidden opacity-60 sm:inline-flex" />}
          </button>
        ))}
      </div>
      {!answer && (
        <div className="flex flex-wrap items-center gap-2 border-t px-2 py-1.5 text-[11px] text-muted-foreground">
          <Button size="sm" className="op-primary h-7 text-xs" disabled={!picked} onClick={() => picked && onAnswer(picked)}>{picked ? <>confirm “{picked}”</> : 'pick an answer'} <Kbd keys="⏎" className="ms-1 opacity-70" /></Button>
          {picked && <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => setPicked(null)}>clear</button>}
          <span className="ms-auto">{hint}</span>
        </div>
      )}
    </div>
  )
}

// ── Sources ────────────────────────────────────────────────────────────

/**
 * What was read, as links into the console's own records. Never an external
 * link unless the tool that produced it was a web search — and then the row
 * says so.
 */
export function AgentSources({ items, className }: { items: { label: string; href: string; note?: string }[]; className?: string }) {
  return (
    <p className={cn('flex flex-wrap items-baseline gap-x-3 gap-y-1 text-[11px] text-muted-foreground', className)}>
      <span className="op-label text-[9px]">sources</span>
      {items.map((s) => (
        <a key={s.href + s.label} href={s.href} className="font-mono">
          {s.label}{s.note && <span className="text-muted-foreground"> · {s.note}</span>}
        </a>
      ))}
    </p>
  )
}

// ── RunAside ───────────────────────────────────────────────────────────

/**
 * The run, as reference facts: which model answered, where it ran, what it
 * was allowed to do, how much context is left, and where the restore points
 * are. The conversation is the main column; this is what is left after it.
 */
export function RunAside({ model, workspace, mode, modeState, context, checkpoints, rows, className }: {
  model: string
  workspace: string
  /** The permission mode, in words: "ask every time", "accept file edits", "auto". */
  mode: string
  /** `warn` when the mode is wider than the default. */
  modeState?: State
  context?: { used: number; max: number }
  checkpoints?: { n: number; at: string }[]
  /** Anything else this run needs stated. Appended after the five. */
  rows?: KV[]
  className?: string
}) {
  const pct = context ? Math.round((context.used / context.max) * 100) : 0
  const last = checkpoints?.[checkpoints.length - 1]
  const all: KV[] = [
    { k: 'model', v: model },
    { k: 'workspace', v: workspace },
    { k: 'mode', v: mode, mono: false, state: modeState },
    ...(context ? [{ k: 'context', v: `${fmtNum(context.used)} / ${fmtNum(context.max / 1000, { digits: 0 })}k · ${pct}%`, state: (pct >= 90 ? 'error' : pct >= 75 ? 'warn' : 'ok') as State }] : []),
    ...(checkpoints ? [{ k: 'checkpoints', v: checkpoints.length === 0 ? 'none yet' : `${checkpoints.length} · last ${last?.at}` }] : []),
    ...(rows ?? []),
  ]
  return <KeyValue rows={all} compact className={className} />
}
