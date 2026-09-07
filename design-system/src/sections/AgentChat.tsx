// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { ArrowUp, Bot, Box, Brain, Check, Container, FileText, Rocket, ShieldCheck, ShieldOff, Zap, Copy, Pencil, X, FilePen, GitBranch, HelpCircle, ListChecks, ListOrdered, Paperclip, RotateCcw, Square, ThumbsDown, ThumbsUp } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Drop, EchoDialog, Kbd, KbdPair, Ledger, MOD, Phrase, Picker, Section, Segmented, Status, StatusLine, TimeChart, type LedgerRow, type Series, type State, type TimePoint } from '@/components/op'
import { AgentGlyph as Glyph, AgentQuestion as Question, AgentRow as Row, AgentSources as Sources, Proposal, Provenance, RunAside, ToolRow as Tool, type ToolState } from '@/components/op'
import { fmtNum } from '@/components/op'
import { PAGE_BLEED } from '@/components/shell-context'
import { cn } from '@/lib/utils'
import { writeToClipboard } from '@/lib/clipboard'

/* ────────────────────────────────────────────────────────────────────────
   /agent — an agentic conversation on v1.

   The vocabulary is Vercel AI Elements (Conversation, Message, Reasoning /
   ChainOfThought, Plan, Tool with its six states, Confirmation, Task, Queue,
   Checkpoint, Sources, Actions, Suggestion, Context, PromptInput) re-drawn
   with the five rules: paper and ink, every border ink, one raised element,
   colour only through the five state glyphs, dense. Every block below is a
   small component so it can move into @temps-sdk/op when the console
   grows an agent surface.

   Rules specific to this surface:
   - the transcript is a ledger of turns; tool calls are LINES, not boxes.
     No borders inside a turn: the icon says what kind of thing it is (a
     terminal for a command, a pen for an edit, a bot for a subagent, a list
     for tasks), the word on the right says its state, and only failure and
     approval colour the icon. Expanded input/output is an inset pane hung
     under the line, indented, no frame
   - a tool call reads as one mono line: icon · name · argument · state
   - approvals are inline, never a modal: approve once · always this session
     · deny. Destructive approvals are red-outlined and say what they destroy
   - a subagent is one collapsible row that holds its own transcript
   - the status line is the agent's verdict: what it is doing right now
   - the prompt bar states model, thinking, permission mode, workspace and
     context in words, each a Picker; the send button is the one ink fill
   ──────────────────────────────────────────────────────────────────────── */

/** A turn: who, when, and the parts. */
function Turn({ who, at, children, model }: { who: 'you' | 'agent'; at: string; children: ReactNode; model?: string }) {
  return (
    <section className={cn('grid gap-2 border-t py-4 first:border-t-0 md:grid-cols-[88px_minmax(0,1fr)]')}>
      <div className="flex items-baseline gap-2 md:block">
        <p className={cn('op-label', who === 'you' && 'text-foreground')}>{who}</p>
        <p className="font-mono text-[10px] text-muted-foreground">{at}{model && <span className="block">{model}</span>}</p>
      </div>
      <div className="min-w-0 space-y-2">{children}</div>
    </section>
  )
}

function Prose({ children }: { children: ReactNode }) {
  return <div className="op-prose max-w-[68ch] space-y-2 text-sm leading-6">{children}</div>
}

function FileChip({ f }: { f: string }) {
  return <span className="font-mono text-[10px] text-muted-foreground">{f}</span>
}

// ── Reasoning ──────────────────────────────────────────────────────────

function Reasoning({ seconds, steps }: { seconds: number; steps: { label: string; state: State; note?: string }[] }) {
  const [open, setOpen] = useState(false)
  const active = steps.find((s) => s.state === 'warn')
  return (
    <Row icon={Brain} state={active ? 'warn' : 'ok'} title={<span className="text-muted-foreground">{active ? `thinking · ${active.label}` : `thought for ${seconds}s`}</span>} meta={`${steps.filter((s) => s.state === 'ok').length}/${steps.length} steps`} open={open} onToggle={() => setOpen((o) => !o)}>
      <ol className="my-1 border-l border-[var(--op-rule-soft)] pl-3">
        {steps.map((s) => (
          <li key={s.label} className="flex items-baseline gap-2 py-1 text-xs">
            <Glyph state={s.state} />
            <span className={cn('min-w-0', s.state === 'idle' && 'text-muted-foreground')}>{s.label}{s.note && <span className="block text-[11px] text-muted-foreground">{s.note}</span>}</span>
          </li>
        ))}
      </ol>
    </Row>
  )
}

// ── Subagent ───────────────────────────────────────────────────────────

function Subagent({ name, model, state, meta, children }: { name: string; model: string; state: State; meta: string; children: ReactNode }) {
  const [open, setOpen] = useState(false)
  return (
    <Row icon={Bot} state={state} open={open} onToggle={() => setOpen((o) => !o)} title={<><span className="font-medium">{name}</span><span className="text-muted-foreground"> · subagent · {model}</span></>} meta={meta}>
      <div className="my-1 space-y-1 border-l border-[var(--op-rule-soft)] pl-3">{children}</div>
    </Row>
  )
}

// ── Plan / Tasks / Question / Approval outcome ─────────────────────────

function Plan({ title, steps, decided, onDecide }: { title: string; steps: { label: string; files?: string[] }[]; decided: 'approved' | 'edited' | null; onDecide: (d: 'approved' | 'edited') => void }) {
  const [open, setOpen] = useState(true)
  return (
    <Row icon={ListOrdered} state={decided ? 'ok' : 'warn'} open={open} onToggle={() => setOpen((o) => !o)} title={<><span className="font-medium">plan</span><span className="text-muted-foreground"> · {title}</span></>} meta={decided ? decided : `${steps.length} steps · waiting for you`}>
      <ol className="my-1">
        {steps.map((s, i) => (
          <li key={s.label} className="grid grid-cols-[16px_minmax(0,1fr)] gap-2 py-1 text-xs">
            <span className="font-mono text-muted-foreground">{i + 1}.</span>
            <span className="min-w-0">{s.label}{s.files && <span className="flex flex-wrap gap-x-3">{s.files.map((f) => <FileChip key={f} f={f} />)}</span>}</span>
          </li>
        ))}
      </ol>
      {!decided && (
        <div className="flex flex-wrap items-center gap-2 py-1 text-xs">
          <Button size="sm" className="op-primary h-7 text-xs" onClick={() => onDecide('approved')}>approve plan <Kbd keys={[MOD, '⏎']} className="ml-1 opacity-70" /></Button>
          <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => onDecide('edited')}>edit steps</Button>
          <span className="ml-auto text-[11px] text-muted-foreground">or just reply with changes</span>
        </div>
      )}
    </Row>
  )
}

type Task = { label: string; state: State; files?: string[] }
function Tasks({ tasks, compact }: { tasks: Task[]; compact?: boolean }) {
  const done = tasks.filter((t) => t.state === 'ok').length
  const [open, setOpen] = useState(true)
  const body = (
    <ol className={cn(!compact && 'my-1')}>
      {tasks.map((t) => (
        <li key={t.label} className="flex items-baseline gap-2 py-1 text-xs">
          <Glyph state={t.state} />
          <span className={cn('min-w-0 flex-1', t.state === 'ok' && 'text-muted-foreground', t.state === 'idle' && 'text-muted-foreground')}>
            <span className={cn(t.state === 'ok' && 'line-through decoration-[var(--op-rule-soft)]')}>{t.label}</span>
            {t.files && <span className="flex flex-wrap gap-x-3">{t.files.map((f) => <FileChip key={f} f={f} />)}</span>}
          </span>
        </li>
      ))}
    </ol>
  )
  if (compact) return body
  return (
    <Row icon={ListChecks} state={done === tasks.length ? 'ok' : 'warn'} open={open} onToggle={() => setOpen((o) => !o)} title={<><span className="font-medium">tasks</span><span className="text-muted-foreground"> · {done} of {tasks.length} done</span></>} meta={tasks.find((t) => t.state === 'warn')?.label}>
      {body}
    </Row>
  )
}

/**
 * A restore point in the transcript. Restoring throws away work, so it is an
 * EchoDialog like every other irreversible action, never a bare link: the row
 * says what it did once it is done, on the row itself.
 */
function Checkpoint({ n, files, at, onRestore }: { n: number; files: number; at: string; onRestore?: () => void }) {
  const [restored, setRestored] = useState(false)
  return (
    <div className="flex items-center gap-3 py-1 text-[11px] text-muted-foreground">
      <span className="h-px flex-1 bg-[var(--op-rule-soft)]" />
      <span className="font-mono">checkpoint {n} · {files} files · {at}</span>
      {restored ? (
        <Status state="ok" label={`restored to ${at}`} className="font-mono" />
      ) : (
        <EchoDialog
          destructive
          trigger={<button type="button" className="underline underline-offset-4 hover:text-foreground">restore</button>}
          title={`Restore checkpoint ${n}`}
          description={`Puts the worktree back as it was at ${at}. Every file the agent changed after this point is discarded; the transcript above it is kept. Type the checkpoint name to confirm.`}
          confirmWord={`checkpoint-${n}`}
          steps={['stash the current changes', `check out checkpoint ${n}`, 'reload the file tree']}
          onDone={() => { setRestored(true); onRestore?.() }}
        />
      )}
      <span className="h-px flex-1 bg-[var(--op-rule-soft)]" />
    </div>
  )
}

/**
 * Every action answers in place, in words, on the button that was pressed.
 * copy → "copied" for 2s (or "couldn't copy", red, when the clipboard is
 * unavailable: plain-http LAN consoles have no navigator.clipboard). retry →
 * "retrying…" and the button locks until the run ends. Thumbs → the pressed
 * one turns ink and says "noted", the other fades; press again to take it back.
 * No toasts: the feedback belongs next to the thing it is about.
 */
function Actions({ onRetry, text, running }: { onRetry: () => void; text: string; running: boolean }) {
  const [copied, setCopied] = useState<'idle' | 'copied' | 'failed'>('idle')
  const [vote, setVote] = useState<'up' | 'down' | null>(null)
  const [retried, setRetried] = useState(false)
  useEffect(() => { if (copied === 'idle') return; const t = setTimeout(() => setCopied('idle'), 2000); return () => clearTimeout(t) }, [copied])
  useEffect(() => { if (!running) setRetried(false) }, [running])
  const b = 'inline-flex h-6 items-center gap-1 px-1.5 text-[11px] text-muted-foreground hover:bg-muted hover:text-foreground disabled:pointer-events-none aria-pressed:text-foreground'
  const copy = async () => { try { await writeToClipboard(text); setCopied('copied') } catch { setCopied('failed') } }
  return (
    <div className="flex flex-wrap items-center gap-1" aria-live="polite">
      <button type="button" className={cn(b, copied === 'failed' && 'text-destructive hover:text-destructive')} onClick={copy}>
        {copied === 'copied' ? <><Check className="h-3 w-3 text-success" /> copied</> : copied === 'failed' ? <><span aria-hidden>×</span> couldn't copy · select the text instead</> : <><Copy className="h-3 w-3" /> copy</>}
      </button>
      <button type="button" className={b} disabled={running} onClick={() => { setRetried(true); onRetry() }}>
        <RotateCcw className={cn('h-3 w-3', running && retried && 'animate-spin')} /> {running && retried ? 'retrying…' : 'retry'}
      </button>
      <button type="button" className={cn(b, vote === 'down' && 'opacity-40')} aria-label="good answer" aria-pressed={vote === 'up'} onClick={() => setVote((v) => (v === 'up' ? null : 'up'))}>
        <ThumbsUp className={cn('h-3 w-3', vote === 'up' && 'fill-current')} />{vote === 'up' && ' noted'}
      </button>
      <button type="button" className={cn(b, vote === 'up' && 'opacity-40')} aria-label="bad answer" aria-pressed={vote === 'down'} onClick={() => setVote((v) => (v === 'down' ? null : 'down'))}>
        <ThumbsDown className={cn('h-3 w-3', vote === 'down' && 'fill-current')} />{vote === 'down' && ' noted · tell me what was wrong below'}
      </button>
    </div>
  )
}

// ── Prompt bar ─────────────────────────────────────────────────────────

const MODELS = [
  { value: 'sonnet-5', label: 'Sonnet 5', meta: 'fast · 200k', group: 'anthropic' },
  { value: 'opus-5', label: 'Opus 5', meta: 'careful · 200k', group: 'anthropic' },
  { value: 'fable-5.1', label: 'Fable 5.1', meta: 'most capable · 1M', group: 'anthropic' },
  { value: 'gpt-5.6', label: 'GPT-5.6', meta: 'via OpenAI key', group: 'other keys' },
]
const THINKING = [{ value: 'off', meta: 'answer directly' }, { value: 'low', meta: 'a few seconds' }, { value: 'high', meta: 'up to a minute' }, { value: 'max', meta: 'as long as it needs' }]
const MODES = [
  { value: 'ask', label: 'Ask every time', meta: 'every tool needs approval', icon: <HelpCircle /> },
  { value: 'edits', label: 'Accept file edits', meta: 'edits run, commands ask', icon: <FilePen /> },
  { value: 'auto', label: 'Auto', meta: 'safe runs; destructive asks', icon: <Zap /> },
  { value: 'full', label: 'Full access', meta: 'nothing asks · sandbox only', state: 'warn' as State, icon: <ShieldOff /> },
]
/* Three different kinds of place, not three flavours of one: brand §6 "an icon
   wherever it adds context" puts the kind in its own slot before the name, so a
   reader tells a worktree from the shared checkout from a container without reading.
   The state glyph keeps its slot — the shared checkout is ◐ and a folder mark, never
   an amber folder. */
const SPACES = [
  { value: 'worktree', label: 'worktree · feat/checkout-address', meta: 'isolated', state: 'ok' as State, icon: <GitBranch /> },
  { value: 'main', label: 'main checkout', meta: 'shared with you', state: 'warn' as State, icon: <Box /> },
  { value: 'sandbox', label: 'sandbox · sbx_9f3', meta: 'docker · fsn1', state: 'ok' as State, icon: <Container /> },
]

/* The console assistant runs in an environment, not a checkout, and its modes
   are the autonomy levels (brand §0), not file-edit permissions. Two different
   agents, so two different vocabularies: the composer must never claim a
   worktree the run aside does not have. */
const CONSOLE_MODES = [
  { value: 'read', label: 'Read only', meta: 'reads run · writes are proposed', icon: <FileText /> },
  { value: 'approve', label: 'Act with approval', meta: 'allowlisted writes ask, then run', icon: <ShieldCheck /> },
  { value: 'observe', label: 'Observe', meta: 'reads only · no writes offered', state: undefined as State | undefined, icon: <HelpCircle /> },
]
const CONSOLE_SPACES = [
  { value: 'production', label: 'checkout-web · production', meta: '3 containers · fsn1', state: 'ok' as State, icon: <Rocket /> },
  { value: 'staging', label: 'checkout-web · staging', meta: '1 container · fsn1', state: 'ok' as State, icon: <Rocket /> },
  { value: 'orders', label: 'orders-api · production', meta: '2 containers · fsn1', state: 'warn' as State, icon: <Rocket /> },
]

/**
 * The context meter and what it opens. The panel is a `Drop`, not a hand-rolled
 * absolute div, so it gets the one phone form, Escape, an outside click and the
 * focus back on the button it came from.
 */
function ContextBadge({ used, max }: { used: number; max: number }) {
  const [open, setOpen] = useState(false)
  const wrap = useRef<HTMLSpanElement>(null)
  const btn = useRef<HTMLButtonElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const pct = Math.round((used / max) * 100)
  const glyph = pct < 50 ? '◔' : pct < 75 ? '◑' : pct < 90 ? '◕' : '●'
  useEffect(() => {
    if (!open) return
    const onDoc = (e: MouseEvent) => { if (!wrap.current?.contains(e.target as Node)) setOpen(false) }
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); setOpen(false); btn.current?.focus() } }
    document.addEventListener('mousedown', onDoc); document.addEventListener('keydown', onKey)
    panel.current?.focus()
    return () => { document.removeEventListener('mousedown', onDoc); document.removeEventListener('keydown', onKey) }
  }, [open])
  return (
    <span ref={wrap} className="relative">
      <button ref={btn} type="button" onClick={() => setOpen((o) => !o)} aria-expanded={open} aria-haspopup="dialog" aria-label={`context: ${fmtNum(used)} of ${fmtNum(max)} tokens, ${pct}%`} className={cn('inline-flex h-7 items-center gap-1.5 px-2 font-mono text-[11px] hover:bg-muted', pct >= 75 ? 'text-warning' : 'text-muted-foreground')}>
        <span aria-hidden>{glyph}</span> {fmtNum(used / 1000, { digits: 1 })}k · {pct}%
      </button>
      <Drop anchor={btn} open={open} side="above" width={256} label="context" className="p-2 font-mono text-[11px]">
        <div ref={panel} tabIndex={-1} className="outline-none">
          <p className="mb-1 flex justify-between"><span>context</span><span>{fmtNum(used)} / {fmtNum(max / 1000, { digits: 0 })}k</span></p>
          <div className="mb-2 h-1 w-full bg-muted"><div className="h-1 bg-foreground" style={{ width: `${pct}%` }} /></div>
          {[['input', 31204, '$0.09'], ['output', 6118, '$0.09'], ['reasoning', 9420, '$0.14'], ['cached', 2083, '$0.00']].map(([k, v, c]) => <p key={String(k)} className="flex justify-between text-muted-foreground"><span>{k}</span><span>{fmtNum(Number(v))} · {c}</span></p>)}
          <p className="mt-1 flex justify-between border-t pt-1"><span>this conversation</span><span>$0.32 · 41 credits</span></p>
          <p className="mt-1 text-muted-foreground">compacts automatically at 90%; <a href="#" onClick={(e) => e.preventDefault()}>compact now</a></p>
        </div>
      </Drop>
    </span>
  )
}

function PromptBar({ value, onChange, running, busy, onSend, onStop, queued, onQueueEdit, onQueueSend, onQueueRemove, scenario }: { value: string; onChange: (v: string) => void; running: boolean; busy: boolean; onSend: () => void; onStop: () => void; queued: string[]; onQueueEdit: (i: number) => void; onQueueSend: (i: number) => void; onQueueRemove: (i: number) => void; scenario: 'coding' | 'console' }) {
  useEffect(() => {
    if (!running) return
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape' && !document.querySelector('[role=dialog]:not(.hidden), [cmdk-root]')) { e.preventDefault(); onStop() } }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [running, onStop])
  const [model, setModel] = useState('sonnet-5')
  const [consoleModel, setConsoleModel] = useState('opus-5')
  const [think, setThink] = useState('high')
  const [mode, setMode] = useState('edits')
  const [space, setSpace] = useState('worktree')
  const [consoleMode, setConsoleMode] = useState('read')
  const [consoleSpace, setConsoleSpace] = useState('production')
  // The two agents share one composer but not one vocabulary: an environment is
  // not a checkout, and an autonomy level is not a file-edit permission.
  const isConsole = scenario === 'console'
  const modes = isConsole ? CONSOLE_MODES : MODES
  const spaces = isConsole ? CONSOLE_SPACES : SPACES
  const modeValue = isConsole ? consoleMode : mode
  const spaceValue = isConsole ? consoleSpace : space
  const modeState = modes.find((m) => m.value === modeValue)?.state
  return (
    <div className="op-sticky-bottom -mx-4 border-t bg-background px-4 pb-3 pt-2 sm:-mx-6 sm:px-6">
      {queued.length > 0 && (
        // Queued messages: no heading, the position under the transcript and above
        // the composer says what they are. Actions are icons that describe what
        // they do (pencil edits, arrow sends, × drops), each with a title.
        <ol className="mb-2 space-y-1" aria-label="queued messages">
          {queued.map((q, i) => (
            <li key={`${q}-${i}`} className="op-inset flex items-center gap-2 py-1.5 pl-3 pr-1.5 text-sm">
              <button type="button" onClick={() => onQueueEdit(i)} title="Edit before it is sent" className="min-w-0 flex-1 truncate text-left">{q}</button>
              <span className="flex shrink-0 items-center gap-0.5">
                <button type="button" onClick={() => onQueueEdit(i)} title="Edit before it is sent" aria-label="edit" className="inline-flex h-7 w-7 items-center justify-center text-muted-foreground hover:bg-muted hover:text-foreground"><Pencil className="h-3.5 w-3.5" /></button>
                <button type="button" onClick={() => onQueueSend(i)} title="Send now · interrupts the current turn" aria-label="send now" className="op-fill-ink inline-flex h-7 w-7 items-center justify-center"><ArrowUp className="h-3.5 w-3.5" /></button>
                <button type="button" onClick={() => onQueueRemove(i)} title="Remove from the queue" aria-label="remove" className="inline-flex h-7 w-7 items-center justify-center text-muted-foreground hover:bg-muted hover:text-foreground"><X className="h-3.5 w-3.5" /></button>
              </span>
            </li>
          ))}
        </ol>
      )}
      <div className="border">
        <textarea value={value} onChange={(e) => onChange(e.target.value)} rows={2} placeholder={running ? 'Type to queue a message; ⏎ sends it after this turn…' : busy ? 'The agent is waiting on you above; ⏎ sends this now…' : 'Message this agent…'} className="block w-full resize-none bg-transparent px-3 py-2 text-sm leading-6 placeholder:text-muted-foreground focus:outline-none"
          onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); onSend() } }} />
        <div className="flex flex-wrap items-center gap-1 border-t px-2 py-1.5">
          <button type="button" className="inline-flex h-7 w-7 items-center justify-center text-muted-foreground hover:bg-muted hover:text-foreground" aria-label="Attach files"><Paperclip className="h-3.5 w-3.5" /></button>
          <Picker value={isConsole ? consoleModel : model} onChange={isConsole ? setConsoleModel : setModel} options={MODELS} mono={false} className="h-7 w-auto border-0 px-2 text-xs hover:bg-muted" width="280px" />
          <Picker value={think} onChange={setThink} options={THINKING.map((t) => ({ ...t, label: `thinking ${t.value}` }))} mono={false} className="h-7 w-auto border-0 px-2 text-xs hover:bg-muted" width="240px" />
          <Picker value={modeValue} onChange={isConsole ? setConsoleMode : setMode} options={modes} mono={false} className={cn('h-7 w-auto border-0 px-2 text-xs hover:bg-muted', modeState === 'warn' && 'text-warning')} width="300px" />
          <Picker value={spaceValue} onChange={isConsole ? setConsoleSpace : setSpace} options={spaces} mono={false} className="h-7 w-auto border-0 px-2 text-xs hover:bg-muted" width="300px" />
          <span className="ml-auto flex items-center gap-1">
            <ContextBadge used={isConsole ? 21480 : 48825} max={200000} />
            {running ? (
              <Button size="sm" variant="outline" className="h-7 text-xs" onClick={onStop}><Square className="h-3 w-3" /> stop <Kbd keys="esc" className="ml-1 opacity-70" /></Button>
            ) : (
              <Button size="sm" className="op-primary h-7 w-7 p-0" onClick={onSend} aria-label="Send" disabled={!value.trim()}><ArrowUp className="h-3.5 w-3.5" /></Button>
            )}
          </span>
        </div>
      </div>
      <p className="mt-1 flex flex-wrap gap-x-3 text-[10px] text-muted-foreground">
        <span><Kbd keys="⏎" /> send</span><span><Kbd keys={['⇧', '⏎']} /> newline</span><span><KbdPair keys={['Y', 'N']} does={['approve', 'deny']} /> the waiting approval</span><span><Kbd keys="esc" /> stop</span>
        <span className="ms-auto">{modes.find((m) => m.value === modeValue)?.meta} · {spaces.find((s) => s.value === spaceValue)?.label}</span>
      </p>
    </div>
  )
}

// ── The conversation ───────────────────────────────────────────────────

// ── Simulated turns ────────────────────────────────────────────────────
// Sending a message runs a fake turn: 3–10 blocks drawn at random from the
// pools below, revealed one at a time. The block under the cursor is "running"
// until the next one appears; questions and destructive commands pause the
// turn until you answer. Queued messages start as soon as a turn ends.

type SimBlock =
  | { kind: 'reasoning'; seconds: number; steps: { label: string; state: State; note?: string }[] }
  | { kind: 'tool'; name: string; arg: string; final: ToolState; ms: number; output?: string; diff?: string; error?: string; gated?: boolean; destructive?: boolean; reason?: string }
  | { kind: 'subagent'; name: string; meta: string; tools: { name: string; arg: string; ms: number; output: string }[]; note: string }
  | { kind: 'tasks'; tasks: Task[] }
  | { kind: 'checkpoint'; files: number }
  | { kind: 'question'; q: string; options: { label: string; note: string }[] }
  | { kind: 'plan'; title: string; steps: { label: string; files?: string[] }[] }
  | { kind: 'prose'; text: string }
type SimTurn = { id: number; prompt: string; at: string; blocks: SimBlock[]; shown: number; done: boolean; stopped?: boolean; answers: Record<number, string>; decisions: Record<number, 'ok' | 'deny' | 'approved' | 'edited'> }

const pick = <T,>(xs: readonly T[]): T => xs[Math.floor(Math.random() * xs.length)]
const between = (a: number, b: number) => a + Math.floor(Math.random() * (b - a + 1))
const clock = () => new Date().toTimeString().slice(0, 5)
const SIM_FILES = ['src/checkout/address.ts', 'src/checkout/AddressForm.tsx', 'src/api/orders.ts', 'src/lib/retry.ts', 'src/routes/webhooks.ts', 'docs/checkout.md', 'src/checkout/constants.ts', 'src/db/migrations/0042_orders_index.sql']
const SIM_CMDS = [
  ['bun test src/checkout', '✓ address.test.ts            8 passed        44ms\n✓ AddressForm.test.tsx      12 passed       371ms\n\n 20 pass · 0 fail · 2 files · 1.02s', 1020],
  ['bunx tsc --noEmit -p .', '', 3900],
  ['bun run lint src/checkout', '✓ 14 files · 0 problems', 860],
  ['git status --short', ' M src/checkout/address.ts\n M src/checkout/address.test.ts\n?? docs/checkout.md', 12],
  ['git log --oneline -3', 'a91f2c3 fix(checkout): guard normalize() for pickup orders\n7d0e1b8 test(checkout): cover empty address\n3c4d5e6 chore: bump undici', 9],
  ['bun run build', 'built in 2.4s · 41 modules · 182 kB', 2400],
] as const
const SIM_FAIL_CMDS = [
  ['bun test src/api', '✗ orders.test.ts             1 failed  0.9s\n\n  ✗ returns 404 for unknown order\n    expected 404, got 500\n    at orders.test.ts:52:19\n\n 11 pass · 1 fail · 1 file · 0.91s', 'exit 1'],
  ['bunx tsc --noEmit -p .', "src/api/orders.ts:88:14 - error TS2339: Property 'line1' does not exist on type 'Address | null'.\n\nFound 1 error.", 'exit 2'],
] as const
const SIM_DIFFS = [
  ['src/api/orders.ts', "@@ -85,3 +85,4 @@ export async function getOrder(id: string) {\n   const order = await db.orders.find(id)\n-  return order\n+  if (!order) throw new NotFound('order', id)\n+  return order"],
  ['docs/checkout.md', "@@ -12,2 +12,5 @@ ## Address handling\n \n+Pickup orders carry no address. `normalize(null)` returns `EMPTY_ADDRESS`\n+so downstream code can read `line1` without a null check.\n+"],
  ['src/lib/retry.ts', "@@ -4,3 +4,3 @@ export function backoff(attempt: number) {\n-  return Math.min(1000 * 2 ** attempt, 30_000)\n+  return Math.min(1000 * 2 ** attempt, 30_000) + Math.random() * 100\n }"],
  ['src/checkout/constants.ts', "@@ -1,2 +1,3 @@\n export const MAX_LINE = 120\n+export const EMPTY_ADDRESS: Address = { line1: '', line2: '', city: '', zip: '', country: '' }\n"],
] as const
const SIM_REASONS = [
  'The stack points at a null read, not at the form', 'Two callers, one of them in a test', 'No migration touches this table since June', 'The docs describe the old behaviour', 'Retry timing uses the real clock', 'Type narrows to null after dep_91a', 'The webhook handler swallows the error', 'Nothing else imports this helper',
]
const SIM_QUESTIONS = [
  { q: 'Two ways to guard this. Which do you prefer?', options: [{ label: 'Return an empty value', note: 'callers keep working; no throw' }, { label: 'Throw a typed error', note: 'callers must handle it; safer long-term' }] },
  { q: 'The lint fix touches 6 files outside the task. Include them?', options: [{ label: 'Only my files', note: 'keep the diff small' }, { label: 'Include them', note: 'one PR, more to review' }, { label: 'Separate PR', note: 'open a chore PR after this one' }] },
  { q: 'Tests take 40s with coverage. Run with or without?', options: [{ label: 'Without', note: 'faster, no report' }, { label: 'With coverage', note: 'slower, attaches the report to the PR' }] },
]
const SIM_PROSE = [
  'Found it in {f}: the value can be null after dep_91a and nothing guards it.',
  'Both callers are covered by tests now; the suite is green.',
  'Docs updated to match the code. No behaviour change.',
  'That failure is unrelated to this change; noted it in the PR instead of fixing it here.',
  'Type check passes after narrowing the parameter.',
]
const SIM_SUMMARY = [
  'Done. {n} steps, {files} file{plural} changed, tests green. Reply to adjust or say "ship it" to open the PR.',
  'Finished. The change is small and covered; nothing outside the task was touched.',
  'That is in place. I stopped short of pushing; tell me when you want the PR opened.',
]

/** Actions that wait for approval. Only the irreversible one is destructive (red); the rest are consequential and ask in ink. */
const GATED = [
  { arg: 'git push -u origin HEAD', output: 'To github.com:acme/api-gateway.git\n * [new branch]  HEAD -> feat/sim', reason: 'Pushes the branch to origin. Visible to the team; the agent cannot unpush, you can.', destructive: false },
  { arg: 'rm -rf node_modules/.cache', output: 'done', reason: 'Deletes the build cache in this worktree. Rebuilds take ~40s longer once.', destructive: false },
  { arg: 'temps deploy api-gateway --env production', output: 'dep_92f queued · production', reason: 'Deploys to production. Traffic shifts on health; rollback is one click.', destructive: false },
  { arg: 'temps db drop staging-copy --yes', output: 'dropped staging-copy (2.1 GB)', reason: 'Drops the database staging-copy, 2.1 GB. It has no backup. This cannot be undone by anyone.', destructive: true },
] as const

function genTurn(id: number, prompt: string): SimTurn {
  const n = between(3, 10)
  const blocks: SimBlock[] = []
  const files = new Set<string>()
  const kinds = ['reasoning', 'read', 'grep', 'cmd', 'cmd', 'edit', 'edit', 'fail', 'fetch', 'subagent', 'tasks', 'checkpoint', 'question', 'plan', 'prose', 'push'] as const
  for (let i = 0; i < n - 1; i++) {
    const k = i === 0 ? pick(['reasoning', 'read', 'grep', 'plan'] as const) : pick(kinds)
    switch (k) {
      case 'reasoning': blocks.push({ kind: 'reasoning', seconds: between(2, 14), steps: Array.from({ length: between(2, 4) }, () => ({ label: pick(SIM_REASONS), state: 'ok' as State })) }); break
      case 'read': { const f = pick(SIM_FILES); blocks.push({ kind: 'tool', name: 'read_file', arg: f, final: 'output-available', ms: between(4, 60), output: `${between(40, 200)} lines · ${between(1, 9)} kB` }); break }
      case 'grep': blocks.push({ kind: 'tool', name: 'grep', arg: `${pick(['normalize(', 'EMPTY_ADDRESS', 'line1', 'backoff(', 'NotFound'])} in src`, final: 'output-available', ms: between(6, 30), output: Array.from({ length: between(1, 4) }, () => `${pick(SIM_FILES)}:${between(3, 140)}`).join('\n') }); break
      case 'cmd': { const c = pick(SIM_CMDS); blocks.push({ kind: 'tool', name: 'run_command', arg: c[0], final: 'output-available', ms: c[2], output: c[1] || '(no output)' }); break }
      case 'fail': { const c = pick(SIM_FAIL_CMDS); blocks.push({ kind: 'tool', name: 'run_command', arg: c[0], final: 'output-error', ms: between(800, 4000), error: `${c[2]}\n${c[1]}` }); break }
      case 'edit': { const left = SIM_DIFFS.filter((d) => !files.has(d[0])); if (!left.length) { i--; break } const d = pick(left); files.add(d[0]); blocks.push({ kind: 'tool', name: 'edit_file', arg: d[0], final: 'output-available', ms: between(2, 9), diff: d[1] }); break }
      case 'fetch': blocks.push({ kind: 'tool', name: 'fetch', arg: pick(['https://registry.npmjs.org/undici', 'https://api.github.com/repos/acme/api-gateway/pulls', 'https://bun.sh/docs/test/coverage']), final: Math.random() < 0.4 ? 'output-error' : 'output-available', ms: between(120, 2400), output: `${between(2, 40)} kB · 200`, error: 'ETIMEDOUT after 30s. Network is off in this worktree; the agent continued without it.' }); break
      case 'subagent': { const c = pick(SIM_CMDS); blocks.push({ kind: 'subagent', name: pick(['test-runner', 'reviewer', 'docs-writer']), meta: `${between(2, 5)} tools · ${between(8, 60)}s · passed`, tools: [{ name: 'run_command', arg: c[0], ms: c[2], output: c[1] || '(no output)' }, { name: 'read_file', arg: pick(SIM_FILES), ms: between(4, 30), output: `${between(40, 200)} lines` }], note: pick(['Nothing to change; the tests agree with the code.', 'Two nits, both fixed in place.', 'Coverage unchanged at 91%.']) }); break }
      case 'tasks': blocks.push({ kind: 'tasks', tasks: [{ label: 'Find the cause', state: 'ok' }, { label: 'Fix it with a test', state: i > n / 2 ? 'ok' : 'warn' }, { label: 'Run the suite', state: i > n / 2 ? 'warn' : 'idle' }, { label: 'Open a PR', state: 'idle' }] }); break
      case 'checkpoint': blocks.push({ kind: 'checkpoint', files: files.size }); break
      case 'question': if (!blocks.some((b) => b.kind === 'question')) blocks.push({ kind: 'question', ...pick(SIM_QUESTIONS) }); else i--; break
      case 'plan': if (!blocks.some((b) => b.kind === 'plan')) blocks.push({ kind: 'plan', title: pick(['Guard the null path', 'Tighten the order lookup', 'Document the pickup case']), steps: [{ label: 'Change the code', files: [pick(SIM_FILES)] }, { label: 'Add a test' }, { label: 'Run the suite' }] }); else i--; break
      case 'prose': blocks.push({ kind: 'prose', text: pick(SIM_PROSE).replace('{f}', pick(SIM_FILES)) }); break
      case 'push': if (!blocks.some((b) => b.kind === 'tool' && b.gated) && i > 1) { const g = pick(GATED); blocks.push({ kind: 'tool', name: 'run_command', arg: g.arg, final: 'output-available', ms: between(900, 3000), output: g.output, gated: true, destructive: g.destructive, reason: g.reason }) } else i--; break
    }
  }
  const nFiles = files.size
  blocks.push({ kind: 'prose', text: pick(SIM_SUMMARY).replace('{n}', String(blocks.length)).replace('{files}', String(nFiles)).replace('{plural}', nFiles === 1 ? '' : 's') })
  return { id, prompt, at: clock(), blocks, shown: 1, done: false, answers: {}, decisions: {} }
}

function SimTurnView({ t, onAnswer, onDecide, onRetry, running }: { t: SimTurn; onAnswer: (i: number, a: string) => void; onDecide: (i: number, d: 'ok' | 'deny' | 'approved' | 'edited') => void; onRetry: () => void; running: boolean }) {
  return (
    <>
      <Turn who="you" at={t.at}><Prose><p>{t.prompt}</p></Prose></Turn>
      <Turn who="agent" at={t.at} model="sonnet-5 · simulated">
        {t.blocks.slice(0, t.shown).map((b, i) => {
          const live = i === t.shown - 1 && !t.done
          switch (b.kind) {
            case 'reasoning': return live ? <Row key={i} icon={Brain} state="idle" title={<span>thinking<span className="op-caret" /></span>} meta="" open={false} onToggle={() => {}} /> : <Reasoning key={i} seconds={b.seconds} steps={b.steps} />
            case 'tool': {
              const decided = t.decisions[i]
              const state: ToolState = b.gated ? (decided === 'deny' ? 'output-denied' : decided === 'ok' ? 'output-available' : 'approval-requested') : live ? (b.name === 'run_command' || b.name === 'fetch' ? 'input-available' : 'input-streaming') : b.final
              return <Tool key={i} name={b.name} arg={b.arg} state={state} ms={b.ms} output={b.output} diff={b.diff} error={b.error} approved={b.gated && decided === 'ok'}
                approval={b.gated ? { destructive: b.destructive, reason: b.reason ?? '', onRespond: (r) => onDecide(i, r === 'deny' ? 'deny' : 'ok') } : undefined} />
            }
            case 'subagent': return <Subagent key={i} name={b.name} model="sonnet-5" state={live ? 'idle' : 'ok'} meta={live ? 'running' : b.meta}>{b.tools.map((x, j) => <Tool key={j} name={x.name} arg={x.arg} state={live && j === b.tools.length - 1 ? 'input-available' : 'output-available'} ms={x.ms} output={x.output} defaultOpen={false} />)}{!live && <Prose><p className="text-xs text-muted-foreground">{b.note}</p></Prose>}</Subagent>
            case 'tasks': return <Tasks key={i} tasks={b.tasks} />
            case 'checkpoint': return <Checkpoint key={i} n={i + 1} files={b.files} at={t.at} />
            case 'question': return <Question key={i} q={b.q} options={b.options} answer={t.answers[i] ?? null} onAnswer={(a) => onAnswer(i, a)} />
            case 'plan': { const d = t.decisions[i]; return <Plan key={i} title={b.title} steps={b.steps} decided={d === 'approved' || d === 'edited' ? d : null} onDecide={(x) => onDecide(i, x)} /> }
            case 'prose': return live && !t.done ? <p key={i} className="text-sm"><span className="op-caret" /></p> : <Prose key={i}><p>{b.text}</p></Prose>
          }
        })}
        {t.stopped && <p className="text-xs text-muted-foreground">Stopped after step {t.shown} of {t.blocks.length}. Reply to continue.</p>}
        {t.done && !t.stopped && <Actions running={running} onRetry={onRetry} text={(t.blocks[t.blocks.length - 1] as { text: string }).text} />}
      </Turn>
    </>
  )
}

// ── Scenario 2: the console assistant ──────────────────────────────────
// The same ledger, answering an operator's question with the console's own
// blocks. Every generated block is wrapped in <Provenance>, so the reader can
// always get from a picture back to the call that drew it; the one write is a
// <Proposal> that does nothing until it is confirmed. Fixture data.

const ERR_HOURS = ['06:00', '06:30', '07:00', '07:30', '08:00', '08:30', '09:00', '09:30', '10:00', '10:30', '11:00', '11:30']
const ERR_RATE = [0.2, 0.3, 0.2, 0.4, 0.3, 0.5, 6.1, 8.4, 9.2, 8.8, 9.6, 9.1]
const ERR_BASE = [0.3, 0.2, 0.3, 0.3, 0.4, 0.4, 0.4, 0.5, 0.4, 0.3, 0.4, 0.4]
const ERR_POINTS: TimePoint[] = ERR_HOURS.map((t, i) => ({ t, rate: ERR_RATE[i], prior: ERR_BASE[i] }))
const ERR_SERIES: Series[] = [
  { key: 'rate', name: 'errors / min', state: 'error' },
  { key: 'prior', name: 'same window, prior week', stroke: 'dotted', weight: 'thin' },
]

const ERR_GROUPS: { id: string; title: string; where: string; events: string; since: string; state: State }[] = [
  { id: 'err_4f21', title: "TypeError: cannot read 'line1'", where: 'checkout-web · AddressForm.tsx:87', events: '1,204', since: 'dep_91a', state: 'error' },
  { id: 'err_39c8', title: 'NotFound: order not found', where: 'orders-api · orders.ts:88', events: '96', since: 'dep_91a', state: 'error' },
  { id: 'err_1a04', title: 'FetchError: ETIMEDOUT registry', where: 'checkout-web · build', events: '11', since: 'dep_8f2', state: 'warn' },
  { id: 'err_0c77', title: 'AbortError: signal aborted', where: 'checkout-web · cart.ts:22', events: '4', since: 'dep_88a', state: 'idle' },
]

const METRICS_QUERY = `temps get_error_time_series
  project      checkout-web
  environment  production
  from         2026-09-07T06:00
  to           2026-09-07T11:45
  bucket       30m
  compare      prior_week`

const GROUPS_QUERY = `temps list_error_groups
  project      checkout-web
  environment  production
  since        dep_91a
  order_by     events desc
  limit        4`

function ConsoleAssistantTurns({ decision, onDecide }: { decision: 'confirmed' | 'declined' | null; onDecide: (d: 'confirmed' | 'declined') => void }) {
  const rows: LedgerRow[] = ERR_GROUPS.map((g) => ({
    id: g.id,
    state: g.state,
    cells: [
      <span key="t" className="min-w-0 truncate">{g.title}</span>,
      <span key="w" className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">{g.where}</span>,
      <Status key="s" state={g.state} label={g.events} className="font-mono" />,
      <span key="d" className="font-mono text-[11px] text-muted-foreground">{g.since}</span>,
    ],
    mobile: (
      <span className="min-w-0">
        <span className="block truncate">{g.title}</span>
        <span className="block truncate font-mono text-[11px] text-muted-foreground">{g.events} events · since {g.since} · {g.where}</span>
      </span>
    ),
  }))
  return (
    <>
      <Turn who="you" at="11:46">
        <Prose><p>Errors on checkout-web jumped this morning. What happened, and what should I do?</p></Prose>
      </Turn>
      <Turn who="agent" at="11:46" model="opus-5 · read only">
        <Prose>
          <p>
            <strong>Roll back checkout-web production to dep_90c.</strong> The error rate went from 0.4 to 9.1 errors per minute at
            09:00, when <span className="font-mono">dep_91a</span> shipped; 1,204 of the 1,315 events since are one group,
            <span className="font-mono"> err_4f21</span>, in <span className="font-mono">AddressForm.tsx:87</span>. Nothing else changed in that window.
          </p>
        </Prose>

        <Tool name="get_error_time_series" arg="checkout-web · production · 06:00→11:45" state="output-available" ms={412} output={'12 buckets · 2 series · 1,315 events'} defaultOpen={false} />
        <Provenance tool="temps get_error_time_series" when="2m ago" range="5h45m · 30m buckets" note="checkout-web · production" query={METRICS_QUERY}>
          <TimeChart
            data={ERR_POINTS}
            series={ERR_SERIES}
            unit="/min"
            title="checkout-web error rate"
            range="06:00 → 11:45 · 30m buckets"
            verdict="Flat at 0.4/min until 09:00, then 9.1/min from dep_91a onward."
            markers={[{ id: 'dep_91a', x: '09:00', at: '09:00', note: 'checkout-web · production' }]}
            thresholds={[{ y: 2, label: 'budget 2/min', state: 'warn' }]}
          />
        </Provenance>

        <Tool name="list_error_groups" arg="checkout-web · production · since dep_91a" state="output-available" ms={188} output={'4 groups · 1,315 events · ordered by events'} defaultOpen={false} />
        <Provenance tool="temps list_error_groups" when="2m ago" range="since dep_91a" note="4 of 4 groups" query={GROUPS_QUERY}>
          <Ledger
            status={null}
            dense
            columns={['error', 'where', { label: 'events', key: 'events', numeric: true }, 'since']}
            grid="1.6fr 1.2fr 100px 90px"
            rows={rows}
            total={ERR_GROUPS.length}
            hint="most events first"
          />
        </Provenance>

        <Proposal
          kind="deploy"
          action="roll back"
          target="checkout-web · production → dep_90c"
          consequence="Production serves dep_90c again within about 40 seconds; the 9.1/min error rate should return to 0.4/min. The three commits in dep_91a stay on main."
          reversal="Reversible: redeploy dep_91a from the deployment page, or ask me to."
          autonomy="act with approval · rollback is on the write allowlist for this project"
          confirmLabel="roll back to dep_90c"
          decided={decision}
          onConfirm={() => onDecide('confirmed')}
          onDecline={() => onDecide('declined')}
        />

        {decision === 'confirmed' && (
          <>
            <Tool name="rollback_to_deployment" arg="checkout-web · production → dep_90c" state="output-available" ms={41200} approved
              output={'dep_93a queued · rollback of dep_91a\nbuild skipped · reusing image sha256:4c1f…\nhealthy at 11:47 · traffic shifted\nerror rate 0.4/min at 11:52'} />
            <Prose><p>Rolled back. <span className="font-mono">dep_93a</span> is serving production and the rate is back to 0.4/min. <span className="font-mono">err_4f21</span> is still open against <span className="font-mono">dep_91a</span> — fix it on a branch and I will watch the redeploy.</p></Prose>
          </>
        )}
        {decision === 'declined' && (
          <Prose><p>Nothing ran. Production stays on <span className="font-mono">dep_91a</span> at 9.1 errors per minute. Tell me to roll back, or ask me to open <span className="font-mono">err_4f21</span> with the stack and the 31 most recent events.</p></Prose>
        )}

        <Sources items={[
          { label: 'err_4f21', href: '#', note: 'error tracking · 1,204 events' },
          { label: 'dep_91a', href: '#', note: 'deployment · 09:00' },
          { label: 'dep_90c', href: '#', note: 'deployment · last healthy' },
        ]} />
      </Turn>
    </>
  )
}

export function AgentChatPage() {
  const [scenario, setScenario] = useState<'coding' | 'console'>('coding')
  const [rollback, setRollback] = useState<'confirmed' | 'declined' | null>(null)
  const [prompt, setPrompt] = useState('')
  const [queued, setQueued] = useState<string[]>([])
  const [plan, setPlan] = useState<'approved' | 'edited' | null>('approved')
  const [answer, setAnswer] = useState<string | null>('Retry with backoff')
  const [pushState, setPushState] = useState<ToolState>('approval-requested')
  const [testState, setTestState] = useState<ToolState>('output-available')
  const [sims, setSims] = useState<SimTurn[]>([])
  const nextId = useRef(1)
  const active = sims.find((t) => !t.done)
  const waitingOn = active && (() => { const b = active.blocks[active.shown - 1]; const i = active.shown - 1; return (b.kind === 'question' && !active.answers[i]) || (b.kind === 'plan' && !active.decisions[i]) || (b.kind === 'tool' && b.gated && !active.decisions[i]) })()
  const startTurn = useCallback((text: string) => { setSims((s) => [...s.map((t) => (t.done ? t : { ...t, done: true, stopped: true })), genTurn(nextId.current++, text)]) }, [])
  // Reveal the next block after a random pause, unless the current block is waiting on the user.
  useEffect(() => {
    if (!active || waitingOn) return
    const id = setTimeout(() => setSims((s) => s.map((t) => (t.id !== active.id ? t : t.shown >= t.blocks.length ? { ...t, done: true } : { ...t, shown: t.shown + 1 }))), between(500, 1400))
    return () => clearTimeout(id)
  }, [active, waitingOn])
  // A turn ended: the run stops, and the first queued message goes next.
  // Tail the transcript. "Pinned" means the reader is at (or near) the bottom; while pinned, every growth of
  // the transcript (a new block, a tool output unfolding, a diff rendering) keeps the bottom in view. Scrolling
  // up unpins; a new turn or a send re-pins. A ResizeObserver on the content is what makes this reliable:
  // scrolling once when state changes misses content that renders a frame later (charts, diffs, images).
  const scroller = useRef<HTMLDivElement>(null)
  const content = useRef<HTMLDivElement>(null)
  const pinned = useRef(true)
  const lastTurnCount = useRef(0)
  useEffect(() => {
    const el = scroller.current, c = content.current
    if (!el || !c) return
    const onScroll = () => { pinned.current = el.scrollHeight - (el.scrollTop + el.clientHeight) < 160 }
    const toBottom = () => { if (pinned.current) el.scrollTop = el.scrollHeight }
    el.addEventListener('scroll', onScroll, { passive: true })
    const ro = new ResizeObserver(toBottom)
    ro.observe(c)
    return () => { el.removeEventListener('scroll', onScroll); ro.disconnect() }
  }, [])
  useEffect(() => {
    if (sims.length !== lastTurnCount.current) { lastTurnCount.current = sims.length; pinned.current = true }
    const el = scroller.current
    if (el && pinned.current) el.scrollTop = el.scrollHeight
  }, [sims])
  // A scripted turn is still open: the coding agent waits on the push approval, the console assistant on the rollback proposal.
  const scriptedRunning = scenario === 'console' ? rollback === null : pushState === 'approval-requested'
  // "busy": a turn is open, so a new message queues. "working": the agent is executing right now, so the bar
  // shows stop. Waiting on an approval or a question is busy but not working: nothing to stop, and stop/esc
  // showing all the time was the bug. Derived, never stored, so it cannot go stale.
  const busy = !!active || scriptedRunning
  const working = !!active && !waitingOn
  useEffect(() => {
    if (active) return
    if (queued.length > 0) { const [head, ...rest] = queued; const id = setTimeout(() => { setQueued(rest); startTurn(head) }, 700); return () => clearTimeout(id) }
  }, [active, sims.length, queued, startTurn])
  /**
   * The badges in the composer hint are real keys. `Y` and `N` answer whichever
   * approval is actually waiting — the scripted push, the rollback proposal, or
   * a gated tool inside a simulated turn — and ⌘⏎ approves an undecided plan.
   * A badge drawn over nothing is the first thing this system bans, and these
   * four were drawn over nothing.
   */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || (e.target as HTMLElement | null)?.isContentEditable) return
      if (document.querySelector('[role=dialog]:not(.hidden), [cmdk-root]')) return
      if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
        if (plan === null) { e.preventDefault(); setPlan('approved') }
        return
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return
      const yes = e.key === 'y' || e.key === 'Y'
      const no = e.key === 'n' || e.key === 'N'
      if (!yes && !no) return
      // The waiting approval inside a simulated turn wins: it is the newest thing on screen.
      if (active && waitingOn) {
        const i = active.shown - 1
        const b = active.blocks[i]
        if (b.kind === 'tool' && b.gated && !active.decisions[i]) {
          e.preventDefault()
          setSims((all) => all.map((t) => (t.id !== active.id ? t : { ...t, decisions: { ...t.decisions, [i]: yes ? 'ok' : 'deny' } })))
          return
        }
        if (b.kind === 'plan' && !active.decisions[i]) {
          e.preventDefault()
          setSims((all) => all.map((t) => (t.id !== active.id ? t : { ...t, decisions: { ...t.decisions, [i]: yes ? 'approved' : 'edited' } })))
          return
        }
        return
      }
      if (scenario === 'console' && rollback === null) { e.preventDefault(); setRollback(yes ? 'confirmed' : 'declined') }
      else if (scenario === 'coding' && pushState === 'approval-requested') { e.preventDefault(); setPushState(yes ? 'approval-responded' : 'output-denied') }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [active, waitingOn, scenario, rollback, pushState, plan])

  const tasks: Task[] = [
    { label: 'Reproduce the null id in AddressForm', state: 'ok', files: ['src/checkout/AddressForm.tsx'] },
    { label: 'Fix normalize() and add a regression test', state: 'ok', files: ['src/checkout/address.ts', 'address.test.ts'] },
    { label: 'Run the checkout test suite', state: 'warn' },
    { label: 'Open a PR against main', state: pushState === 'approval-responded' ? 'warn' : 'idle' },
  ]
  const consoleStatus = rollback === 'confirmed'
    ? <StatusLine state="ok">Rolled back to dep_90c. checkout-web production is at 0.4 errors per minute.</StatusLine>
    : rollback === 'declined'
      ? <StatusLine state="warn">Nothing ran. checkout-web production is still on dep_91a at 9.1 errors per minute.</StatusLine>
      : <StatusLine state="warn" more={{ label: '1 proposal waiting', onClick: () => document.getElementById('proposal')?.scrollIntoView({ block: 'center' }) }}>Read only; <Phrase onClick={() => document.getElementById('proposal')?.scrollIntoView({ block: 'center' })}>one rollback is proposed</Phrase> and waiting for you.</StatusLine>
  const status = scenario === 'console' && sims.length === 0
    ? consoleStatus
    : active
    ? <StatusLine state={waitingOn ? 'warn' : 'idle'} more={{ label: `step ${active.shown} of ${active.blocks.length}` }}>{waitingOn ? <><Phrase onClick={() => document.getElementById(`sim-${active.id}`)?.scrollIntoView({ block: 'end' })}>Waiting for your answer</Phrase> before continuing.</> : <>Working on “{active.prompt.length > 40 ? active.prompt.slice(0, 40) + '…' : active.prompt}”.</>}</StatusLine>
    : sims.length > 0
      ? <StatusLine state="ok">Finished. Reply to continue{queued.length ? `, ${queued.length} queued` : ''}.</StatusLine>
      : scriptedRunning
    ? <StatusLine state="warn" more={{ label: pushState === 'approval-requested' ? '1 approval waiting' : '', onClick: () => document.getElementById('approval')?.scrollIntoView({ block: 'center' }) }}>Tests passed; <Phrase onClick={() => document.getElementById('approval')?.scrollIntoView({ block: 'center' })}>waiting for your approval</Phrase> to push and open the PR.</StatusLine>
    : <StatusLine state="ok">{pushState === 'output-denied' ? 'Stopped before the push. Reply to continue.' : 'PR #482 is open. Reply to continue.'}</StatusLine>

  return (
    // Fills the viewport under the docs header (h-14): the transcript column scrolls, the rail stays put.
    <div className={cn('operator ink v1 flex h-[calc(100dvh-3rem)] flex-col', PAGE_BLEED)}>
      <div className="shrink-0 border-b px-4 py-3 text-xs sm:px-6">
        <div className="flex flex-wrap items-center gap-3">
          <p className="op-label">agent · an agentic conversation on v1</p>
          <Segmented
            className="ms-auto"
            value={scenario}
            onChange={setScenario}
            options={[['coding', 'coding agent'], ['console', 'console assistant']] as const}
          />
        </div>
        <p className="op-prose mt-1 max-w-3xl text-sm text-muted-foreground">
          Two surfaces, one ledger: a <strong>coding agent</strong> in a worktree, and the <strong>console assistant</strong> answering an
          operator with generated blocks under <Link to="/guide#generative-ui" className="underline underline-offset-4">the generative-UI rules</Link> —
          every block carries the call that drew it, and the one write is a proposal.
          The <a href="https://elements.ai-sdk.dev" className="underline underline-offset-4">AI Elements</a> vocabulary drawn with the v1 rules: tool calls are rows,
          approvals are inline, the status line says what the agent is doing, the prompt bar says model, thinking, mode and workspace in words.
          Everything here is clickable. Console at <Link to="/v1" className="underline underline-offset-4">/v1</Link>; components at <Link to="/op-components" className="underline underline-offset-4">/op-components</Link>.
        </p>
      </div>

      <div className="grid min-h-0 flex-1 xl:grid-cols-[minmax(0,1fr)_280px]">
        {/* Conversation: the one scrolling column */}
        <div ref={scroller} className="relative flex min-h-0 min-w-0 flex-col overflow-y-auto px-4 sm:px-6">
          <div className="flex flex-wrap items-baseline gap-x-2 pt-4">
            <h1 className="op-title">{scenario === 'console' ? 'why did checkout errors jump?' : 'fix: address form null id'}</h1>
            <p className="font-mono text-[11px] text-muted-foreground">{scenario === 'console'
              ? `checkout-web · production · started 11:46 · ${1 + sims.length} turns`
              : `api-gateway · worktree feat/checkout-address · started 14:02 · ${3 + sims.length} turns`}</p>
          </div>
          <div className="mt-3">{status}</div>

          <div ref={content} className="flex-1 pb-6">
            {scenario === 'console' ? (
              <div id="proposal"><ConsoleAssistantTurns decision={rollback} onDecide={setRollback} /></div>
            ) : (
            <>
            <Turn who="you" at="14:02">
              <Prose><p>Checkout is throwing <span className="font-mono">TypeError: cannot read 'line1'</span> in AddressForm since dep_91a, 31 events. Find the cause, fix it with a test, run the suite, and open a PR. Don't touch the Stripe retry code.</p></Prose>
              <p className="flex flex-wrap gap-x-3 text-[11px]"><span className="op-label text-[9px]">attached</span><FileChip f="err_4f21 · 31 events" /><FileChip f="dep_91a" /></p>
            </Turn>

            <Turn who="agent" at="14:02" model="sonnet-5 · thinking high">
              <Reasoning seconds={6} steps={[
                { label: 'Read the error: line1 read on undefined', state: 'ok' },
                { label: 'The stack points at normalize() in address.ts, called with the raw form value', state: 'ok' },
                { label: 'dep_91a changed the form to allow an empty address for pickup orders', state: 'ok', note: 'so normalize() now receives null for the first time' },
              ]} />
              <Tool name="read_file" arg="src/checkout/AddressForm.tsx" state="output-available" ms={38} input={'{ "path": "src/checkout/AddressForm.tsx", "lines": "60-110" }'} output={'  86  const addr = form.values.address\n  87  const normalized = normalize(addr)   // addr is null for pickup\n  88  return normalized.line1.trim()'} />
              <Tool name="grep" arg="normalize( in src/checkout" state="output-available" ms={12} output={'src/checkout/AddressForm.tsx:87\nsrc/checkout/address.ts:14\nsrc/checkout/address.test.ts:9'} />
              <Plan title="Guard normalize() and cover pickup orders" decided={plan} onDecide={setPlan} steps={[
                { label: 'Return an empty Address from normalize(null) instead of throwing', files: ['src/checkout/address.ts'] },
                { label: 'Add a regression test for pickup orders with no address', files: ['src/checkout/address.test.ts'] },
                { label: 'Run the checkout suite; leave Stripe retry untouched' },
                { label: 'Open a PR against main with the error id in the description' },
              ]} />
            </Turn>

            <Checkpoint n={1} files={0} at="14:03" />

            <Turn who="agent" at="14:03" model="sonnet-5">
              <Tool name="edit_file" arg="src/checkout/address.ts" state="output-available" ms={4} diff={'@@ -12,4 +12,6 @@ import { EMPTY_ADDRESS } from \'./constants\'\n-export function normalize(a: RawAddress): Address {\n+export function normalize(a: RawAddress | null): Address {\n+  if (!a) return EMPTY_ADDRESS\n   return {\n     line1: a.line1?.trim() ?? \'\','} />
              <Tool name="edit_file" arg="src/checkout/address.test.ts" state="output-available" ms={3} diff={'@@ -7,3 +7,7 @@ describe(\'normalize\', () => {\n   it(\'trims line1\', () => {\n     expect(normalize({ line1: \' 12 Main \' }).line1).toBe(\'12 Main\')\n   })\n+  it(\'returns an empty address for pickup orders\', () => {\n+    expect(normalize(null)).toEqual(EMPTY_ADDRESS)\n+  })'} />
              <Tool name="run_command" arg="bun test src/checkout --reporter=dots" state={testState} ms={2140} approved
                approval={{ reason: 'Runs a command in the worktree. Tests only read files and write to /tmp.', onRespond: (r) => setTestState(r === 'deny' ? 'output-denied' : 'output-available') }}
                output={'✓ address.test.ts            7 passed        41ms\n✓ AddressForm.test.tsx      12 passed       380ms\n✗ stripe.retry.test.ts       2 passed  1 failed  1.7s\n\n  ✗ retries once on 429\n    expected 2 calls, got 1\n    at stripe.retry.test.ts:31:22\n\n 21 pass · 1 fail · 3 files · 2.14s'} />
              <Tool name="fetch" arg="https://registry.npmjs.org/undici" state="output-error" error="ETIMEDOUT after 30s. Network is off in this worktree; the agent continued without it." />
              <Question q="stripe.retry.test.ts fails and I was told not to touch it. How should I proceed?" answer={answer} onAnswer={setAnswer} options={[
                { label: 'Retry with backoff', note: 'run it again; if it passes, note it as flaky in the PR' },
                { label: 'Skip it', note: 'mark it skipped with a TODO and open an issue' },
                { label: 'Investigate', note: 'read the retry code, but change nothing' },
                { label: 'Stop here', note: 'leave the branch as it is' },
              ]} />
            </Turn>

            <Turn who="agent" at="14:05" model="sonnet-5">
              <Subagent name="test-runner" model="sonnet-5" state="ok" meta="3 tools · 41s · passed">
                <Tool name="run_command" arg="bun test src/checkout/stripe.retry.test.ts --retry 2" state="output-available" ms={38900} output={'✗ retries once on 429  (attempt 1)  expected 2 calls, got 1\n✓ retries once on 429  (attempt 2)  1.9s\n✓ gives up after 5     0.4s\n✓ surfaces the last error\n\n 3 pass · 0 fail · retried 1 · 38.9s'} />
                <Tool name="read_file" arg="src/checkout/stripe.retry.ts" state="output-available" ms={9} output={'// uses Date.now() for backoff timing; not faked in the test → flaky under load'} />
                <Prose><p className="text-xs text-muted-foreground">Flaky, not broken: the test times a real backoff. Nothing changed in retry code. Noted for the PR.</p></Prose>
              </Subagent>
              <Tasks tasks={tasks} />
              <div id="approval">
                <Tool name="run_command" arg="git push -u origin feat/checkout-address && gh pr create --fill" state={pushState} ms={1800}
                  approval={{ reason: 'Pushes 2 commits to origin and opens a pull request on acme/api-gateway. Visible to the team; the agent cannot unpush, you can.', onRespond: (r) => setPushState(r === 'deny' ? 'output-denied' : 'approval-responded') }}
                  output={'Enumerating objects: 11, done.\nWriting objects: 100% (7/7), 1.42 KiB · 0 bytes/s, done.\nTo github.com:acme/api-gateway.git\n * [new branch]      feat/checkout-address -> feat/checkout-address\nbranch \'feat/checkout-address\' set up to track \'origin/feat/checkout-address\'.\n\nCreating pull request for feat/checkout-address into main in acme/api-gateway\nhttps://github.com/acme/api-gateway/pull/482'} />
              </div>
              {pushState === 'approval-responded' && (
                <>
                  <Prose>
                    <p>Fixed. <span className="font-mono">normalize()</span> now returns an empty address for pickup orders instead of reading <span className="font-mono">line1</span> on null, with a regression test. The checkout suite passes; the Stripe retry test was flaky (real backoff timing), passed on retry, and is untouched. PR <a href="#" onClick={(e) => e.preventDefault()}>#482</a> is open against main and references <span className="font-mono">err_4f21</span>.</p>
                  </Prose>
                  <Sources items={[{ label: 'err_4f21 · error tracking', href: '#' }, { label: 'dep_91a · deploy', href: '#' }, { label: 'PR #482', href: '#' }]} />
                  <Actions running={working} onRetry={() => startTurn('Retry the last step')} text="Fixed. normalize() now returns an empty address for pickup orders instead of reading line1 on null, with a regression test. The checkout suite passes; the Stripe retry test was flaky (real backoff timing), passed on retry, and is untouched. PR #482 is open against main and references err_4f21." />
                </>
              )}
              {pushState === 'output-denied' && <Prose><p>Understood, not pushing. The branch is ready in the worktree; push it yourself with <span className="font-mono">git push -u origin feat/checkout-address</span> or tell me what to change first.</p></Prose>}
            </Turn>
            </>
            )}

            {sims.map((t) => (
              <div key={t.id} id={`sim-${t.id}`}>
                <SimTurnView t={t} running={working} onRetry={() => startTurn(t.prompt)}
                  onAnswer={(i, a) => setSims((s) => s.map((x) => (x.id === t.id ? { ...x, answers: { ...x.answers, [i]: a } } : x)))}
                  onDecide={(i, d) => setSims((s) => s.map((x) => (x.id === t.id ? { ...x, decisions: { ...x.decisions, [i]: d } } : x)))} />
              </div>
            ))}

            {sims.length === 0 && (scenario === 'console' || pushState === 'approval-requested') && (
              <div className="flex flex-wrap gap-2 pt-2">
                <span className="op-label self-center text-[9px]">try</span>{(scenario === 'console'
                  ? ['Show me the stack for err_4f21', 'Compare this week with last', 'Which nodes served the errors?']
                  : ['Add a changelog entry', 'Explain the Stripe flakiness', 'Run the full suite instead']).map((s) => <button key={s} type="button" onClick={() => setPrompt(s)} className="px-2 py-1 text-xs text-muted-foreground underline decoration-[var(--op-rule-soft)] underline-offset-4 hover:text-foreground">{s}</button>)}
              </div>
            )}
          </div>

          <PromptBar value={prompt} onChange={setPrompt} running={working} busy={busy} queued={queued} scenario={scenario}
            onSend={() => { if (!prompt.trim()) return; if (working) setQueued((q) => [...q, prompt.trim()]); else startTurn(prompt.trim()); setPrompt('') }}
            onStop={() => setSims((s) => s.map((t) => (t.done ? t : { ...t, done: true, stopped: true })))}
            onQueueEdit={(i) => { setPrompt(queued[i]); setQueued((q) => q.filter((_, j) => j !== i)) }}
            onQueueSend={(i) => { const m = queued[i]; setQueued((q) => q.filter((_, j) => j !== i)); startTurn(m) }}
            onQueueRemove={(i) => setQueued((q) => q.filter((_, j) => j !== i))} />
        </div>

        {/* Rail */}
        <aside className="hidden min-h-0 min-w-0 overflow-y-auto border-l px-4 py-4 text-xs xl:block">
          <div>
            {/* The run, always first: which model answered, where, under what
                policy, how much context is left, where the restore points are. */}
            <Section title="Run" meta={scenario === 'console' ? '11:46 → now' : '14:02 → now'}>
              {scenario === 'console' ? (
                <RunAside
                  model="opus-5"
                  workspace="checkout-web · production"
                  mode="read only · writes propose"
                  context={{ used: 21480, max: 200000 }}
                  rows={[
                    { k: 'tools', v: '2 calls · 0 failed' },
                    { k: 'proposals', v: rollback ? `1 ${rollback}` : '1 waiting', state: rollback === 'confirmed' ? 'ok' : rollback === 'declined' ? 'idle' : 'warn' },
                    { k: 'cost', v: '$0.04 · 5 credits' },
                  ]}
                />
              ) : (
                <RunAside
                  model="sonnet-5 · thinking high"
                  workspace="worktree feat/checkout-address"
                  mode="accept file edits"
                  modeState="warn"
                  context={{ used: 48825, max: 200000 }}
                  checkpoints={[{ n: 1, at: '14:03' }]}
                  rows={[
                    { k: 'tools', v: '9 calls · 1 failed' },
                    { k: 'subagents', v: '1 · test-runner' },
                    { k: 'approvals', v: pushState === 'approval-requested' ? '1 waiting' : '2 answered', state: pushState === 'approval-requested' ? 'warn' : 'ok' },
                    { k: 'cost', v: '$0.32 · 41 credits' },
                  ]}
                />
              )}
            </Section>
            {scenario === 'console' ? (
              <Section title="Autonomy" meta="per capability">
                <div className="space-y-1 text-[11px] text-muted-foreground">
                  <p className="flex items-center gap-2"><Status state="ok" label="read analytics, errors, traces" /> observe · auto</p>
                  <p className="flex items-center gap-2"><Status state="ok" label="read env var names" /> observe · values masked</p>
                  <p className="flex items-center gap-2"><Status state="warn" label="roll back, restart, pause" /> act with approval</p>
                  <p className="flex items-center gap-2"><Status state="idle" label="delete anything" /> not on the allowlist</p>
                </div>
              </Section>
            ) : (
            <>
            <Section title="Tasks" meta={`${tasks.filter((t) => t.state === 'ok').length} of ${tasks.length} done`}>
              <Tasks tasks={tasks} compact />
            </Section>
            <Section title="Files changed" meta="2 · +7 −1">
              <div>
                {[['src/checkout/address.ts', '+3 −1'], ['src/checkout/address.test.ts', '+4']].map(([f, d]) => <p key={f} className="flex items-center gap-2 py-1 font-mono text-[11px]"><FilePen className="h-3 w-3 shrink-0 text-muted-foreground" /><span className="min-w-0 flex-1 truncate">{f}</span><span className="text-muted-foreground">{d}</span></p>)}
              </div>
              <p className="mt-1 text-[11px] text-muted-foreground">in worktree feat/checkout-address · <a href="#" onClick={(e) => e.preventDefault()}>open diff</a></p>
            </Section>
            <Section title="Permissions" meta="Accept file edits">
              <div className="space-y-1 text-[11px] text-muted-foreground">
                <p className="flex items-center gap-2"><Status state="ok" label="read files, grep" /> auto</p>
                <p className="flex items-center gap-2"><Status state="ok" label="edit files" /> auto</p>
                <p className="flex items-center gap-2"><Status state="warn" label="run_command" /> asks · bun test allowed this session</p>
                <p className="flex items-center gap-2"><Status state="error" label="git push, rm, deploy" /> always asks</p>
              </div>
            </Section>
            </>
            )}
          </div>
        </aside>
      </div>
    </div>
  )
}
