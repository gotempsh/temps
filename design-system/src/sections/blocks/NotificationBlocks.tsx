// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { toast } from 'sonner'
import { Block, Demo, Rule } from '@/components/op-doc'
import { AttentionHost, Callout, EchoDialog, GLYPH, GLYPH_CLASS, Phrase, ShellSlotsProvider, StatusLine } from '@/components/op'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   Notification taxonomy (docs/notifications.md). Five surfaces, and every
   message belongs to exactly one of them. The table below is the decision,
   with the live surface beside each row so the choice can be compared
   rather than described.

   Fictional instance throughout: temps.acme.sh.
   ──────────────────────────────────────────────────────────────────────── */

const SKIN = 'operator ink v1'

/**
 * The sandbox's toast contract, the same shape the console's `useNotify`
 * wraps: state · headline · fact, with an optional undo. The package ships no
 * toast; toasts are `sonner`, and the skin class goes to the portal like every
 * other portalled surface.
 */
type Level = 'ok' | 'warn' | 'err'
const TONE: Record<Level, string> = { ok: 'text-success', warn: 'text-warning', err: 'text-destructive' }
function notify(level: Level, msg: string, detail?: string, undo?: () => void) {
  toast.custom(
    (id) => (
      <div className={cn(SKIN, 'flex w-full items-start gap-2 border bg-background px-3 py-2 font-mono text-xs')}>
        <span className={cn('w-8 shrink-0', TONE[level])}>{level}</span>
        <span className="min-w-0 flex-1">
          <span className="block">{msg}</span>
          {detail && <span className="block truncate text-muted-foreground">{detail}</span>}
        </span>
        {undo && <button type="button" className="shrink-0 underline underline-offset-4" onClick={() => { undo(); toast.dismiss(id) }}>undo</button>}
      </div>
    ),
    { duration: 6000 },
  )
}

/** What a toast looks like without waiting for one: the same row, on the page. */
function ToastStill({ level, msg, detail, undo }: { level: Level; msg: string; detail?: string; undo?: boolean }) {
  return (
    <div className="flex items-start gap-2 border bg-background px-3 py-2 font-mono text-xs">
      <span className={cn('w-8 shrink-0', TONE[level])}>{level}</span>
      <span className="min-w-0 flex-1">
        <span className="block">{msg}</span>
        {detail && <span className="block truncate text-muted-foreground">{detail}</span>}
      </span>
      {undo && <span className="shrink-0 underline underline-offset-4">undo</span>}
    </div>
  )
}

/** The header's attention control with two real StatusLines portalled into it. */
function BellDemo() {
  const [attention, setAttention] = useState<HTMLElement | null>(null)
  return (
    <div className="flex items-center gap-3 border px-3 py-2">
      <AttentionHost onSlot={setAttention} />
      <span className="text-[11px] text-muted-foreground">counted by state, with a number. Click it.</span>
      <ShellSlotsProvider value={{ crumb: null, attention }}>
        <StatusLine state="error" more={{ label: '+1 warning', items: [{ state: 'warn', children: <><Phrase>api-gateway</Phrase> error rate 0.61% since dep_91a.</> }] }}>
          <Phrase>billing-worker</Phrase> is failing health checks.
        </StatusLine>
      </ShellSlotsProvider>
    </div>
  )
}

// ── the decision table ─────────────────────────────────────────────────

type Row = { trigger: string; surface: string; persistence: string; severity: string; action: string; example: ReactNode }

const ROWS: Row[] = [
  {
    trigger: 'A verdict about the page the reader is on',
    surface: 'StatusLine (into the header\'s attention slot; inline outside a shell)',
    persistence: 'while the page is open',
    severity: 'ok · warn · error · idle · sampled',
    action: 'one Phrase, on the thing to act on',
    example: (
      <StatusLine state="error" sticky={false} more={{ label: '+1 warning', items: [{ state: 'warn', children: <><Phrase>api-gateway</Phrase> error rate 0.61% since dep_91a.</> }] }}>
        <Phrase>billing-worker</Phrase> is failing health checks.
      </StatusLine>
    ),
  },
  {
    trigger: 'A fault or warning that belongs to one thing on the page',
    surface: 'Callout, inline directly above what it applies to',
    persistence: 'until it is fixed — it is state, not an event',
    severity: 'error · warn (ok only when it carries proof)',
    action: 'one action, right of the text',
    example: (
      <Callout state="error" title="The git connection to github/acme expired"
        quote="401 Bad credentials · GET /repos/acme/api-gateway"
        action={<Button size="sm" variant="outline" className="h-7 text-xs">reconnect</Button>}>
        Pushes to main stopped deploying 2 days ago; 4 commits are waiting. Reconnecting replays them.
      </Callout>
    ),
  },
  {
    trigger: 'The result of an action the reader just took',
    surface: 'toast · notify(state, headline, fact, undo?)',
    persistence: '≤ 6s, auto-dismiss',
    severity: 'ok · warn · err',
    action: 'an undo, and never the only one',
    example: (
      <div className="space-y-2">
        <ToastStill level="ok" msg="api-gateway deploying" detail="dep_93a · main@9bc61c0" />
        <ToastStill level="ok" msg="rolled back to dep_90e" detail="traffic switched · 41 routes" undo />
        <div className="flex flex-wrap gap-2 pt-1">
          <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'api-gateway deploying', 'dep_93a · main@9bc61c0')}>fire one</Button>
          <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'rolled back to dep_90e', 'traffic switched · 41 routes', () => notify('ok', 'rollback undone', 'dep_91a is serving again'))}>fire one with undo</Button>
        </div>
      </div>
    ),
  },
  {
    trigger: 'Something that happened while the reader was elsewhere',
    surface: 'the bell · AttentionHost, counted by state',
    persistence: 'until read',
    severity: 'error and warn counted; ok is the quiet glyph',
    action: 'each entry links to its page',
    example: <BellDemo />,
  },
  {
    trigger: 'A decision that must be made before anything else happens',
    surface: 'EchoDialog',
    persistence: 'until answered',
    severity: 'red only when the loss is irreversible',
    action: 'the action is the whole point',
    example: (
      <EchoDialog destructive skin={SKIN}
        trigger={<Button size="sm" variant="outline" className="h-7 border-destructive text-xs text-destructive">remove mail.acme.sh</Button>}
        title="Remove mail.acme.sh"
        description="Mail from mail.acme.sh stops sending immediately; 1,284 emails were sent from it in the last 30 days. DNS records are yours and stay where they are."
        confirmWord="mail.acme.sh"
        steps={['remove identity from provider', 'delete domain', 'reject new sends']}
        onDone={() => notify('warn', 'mail.acme.sh removed', '3 DNS records are now unused')} />
    ),
  },
]

function DecisionTable() {
  return (
    <div className="op-rows @container border">
      <div className="hidden gap-4 px-4 py-2 @3xl:grid @3xl:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)]">
        <span className="op-label">trigger · surface · persistence · severity · action</span>
        <span className="op-label">in the console</span>
      </div>
      {ROWS.map((r) => (
        <div key={r.surface} className="grid gap-4 py-4 text-xs @3xl:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)]">
          <div className="min-w-0 px-4">
            <p className="font-medium">{r.trigger}</p>
            <p className="mt-1 text-muted-foreground">
              <span className="block text-foreground">{r.surface}</span>
              <span className="block">{r.persistence}</span>
              <span className="block">{r.severity}</span>
              <span className="block">{r.action}</span>
            </p>
          </div>
          {/* px-4 so a surface that bleeds to its container's edge (StatusLine) lands on the cell's edge. */}
          <div className="min-w-0 px-4 sm:px-6">{r.example}</div>
        </div>
      ))}
    </div>
  )
}

// ── the section ────────────────────────────────────────────────────────

export function NotificationBlocks() {
  return (
    <>
      <Block id="notify-table" title="Which surface says it" api={`verdict about the page    → StatusLine
fault in context          → Callout
result of an action       → toast
happened while away       → the bell
a blocking decision       → EchoDialog`}
        rule={<>
          <p>Five surfaces carry messages and every message belongs to exactly one. Pick by asking what kind of message it is, not by what is convenient to call from the handler.</p>
          <p>One surface per message: never a toast and a Callout for the same event. A fault that persists is a Callout — a toast that has to be read is gone before the reader looks up.</p>
          <Rule state="ok">Each row here renders its real surface, so the choice can be compared rather than described.</Rule>
          <Rule state="error">A toast announcing that the git connection expired. It will expire again tomorrow and the toast will be gone.</Rule>
        </>}>
        <Demo label="the decision, with a live example per row">
          <DecisionTable />
        </Demo>
      </Block>

      <Block id="notify-toast" title="Toasts" api={`notify(level, msg, detail?, undo?)
//     ok|warn|err
//            ≤ 6 words, names the object
//                 the fact: an id, a count, a scope
//                          optional; never the only undo`}
        rule={<>
          <p>The package ships no toast component: toasts are <code>sonner</code>, mounted once, and the console wraps them in one hook so every caller writes the same shape — state · headline · fact.</p>
          <p>Six words or fewer, and name the object: <span className="font-mono">api-gateway deploying · dep_93a</span>. Never "success", "done" or "error" alone; a message that names nothing proves nothing.</p>
          <p>Severity words are <span className="font-mono">ok</span> / <span className="font-mono">warn</span> / <span className="font-mono">error</span> and their glyphs, nowhere else. Pass the skin class to the portal.</p>
          <Rule state="ok">A cheap change is confirmed by its consequence: it happens, and the toast carries the way back.</Rule>
          <Rule state="error">A toast on page load, a toast per failed row, or a toast that holds the only undo.</Rule>
        </>}>
        <Demo label="the shape · the same rows, still">
          <div className="space-y-2">
            <ToastStill level="ok" msg="settings saved" detail="applies to emails sent from now on" />
            <ToastStill level="ok" msg="test email sent" detail="mail.acme.sh → maya@acme.sh · accepted by SES" />
            <ToastStill level="warn" msg="tracking disabled" detail="653 events deleted" />
            <ToastStill level="err" msg="deploy failed" detail="dep_93a · build exited 1 at step 3" />
          </div>
        </Demo>
        <Demo label="wrong">
          <div className="space-y-2">
            <ToastStill level="ok" msg="Success!" />
            <ToastStill level="err" msg="An error occurred. Please try again." />
            <p className="text-[11px] text-muted-foreground">Neither names an object, and the second is a fault that will still be true in six seconds: that one is a Callout.</p>
          </div>
        </Demo>
      </Block>

      <Block id="notify-attention" title="The bell" api={`<AttentionHost onSlot={setSlot} />
// × 2 ◐ 1 — counted by state, with the number
// quiet: one green glyph, no number`}
        rule={<>
          <p>What happened while the reader was elsewhere is counted in the header, by state, with the same glyphs the rest of the console uses. <span className="font-mono">× 2 ◐ 1</span> is a sentence; a badge is not.</p>
          <p>Unread is a count. A red dot with no number tells the reader to go looking, which is the one thing a notification exists to prevent. Nothing wrong is one quiet glyph and no number.</p>
          <p>Nothing here moves the layout. No banner pushes the page down; the only thing that takes a line of the page to say something is the Settings sticky save bar, which is the form's own state.</p>
          <Rule state="ok">The StatusLines portal into it, so a verdict and a count are the same list.</Rule>
          <Rule state="error">A red dot with no count. A banner above the header.</Rule>
        </>}>
        <Demo label="live">
          <BellDemo />
        </Demo>
        <Demo label="quiet · nothing needs attention">
          <div className="flex items-center gap-3 border px-3 py-2">
            <span className="flex h-7 items-center gap-2 border px-2 font-mono text-[11px] text-muted-foreground">
              <span aria-hidden className={GLYPH_CLASS.ok}>{GLYPH.ok}</span>
            </span>
            <span className="text-[11px] text-muted-foreground">Zero is not an alarm.</span>
          </div>
        </Demo>
      </Block>
    </>
  )
}
