// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { useShellSlots } from './shell-slots'
import { Drop } from './drop'
import { cn } from './lib/cn'

/**
 * The five states a thing can be in. Colour is only ever applied through
 * these, and always next to a glyph and a word.
 *
 *  ok       ●  green   healthy, deployed, passing
 *  warn     ◐  amber   degraded, above threshold, expiring
 *  error    ×  red     failing, unreachable
 *  idle     ○  muted   not deployed, not configured, nothing yet
 *  sampled  ◌  muted   telemetry head-sampled past the plan allowance.
 *                      From pricing.md: "the console says so; it is never
 *                      silently dropped."
 */
export type State = 'ok' | 'warn' | 'error' | 'idle' | 'sampled'
export const GLYPH: Record<State, string> = { ok: '●', warn: '◐', error: '×', idle: '○', sampled: '◌' }
export const GLYPH_CLASS: Record<State, string> = {
  ok: 'text-success',
  warn: 'text-warning',
  error: 'text-destructive',
  idle: 'text-muted-foreground',
  sampled: 'text-muted-foreground',
}
/** Sort order when a list is "needs attention first". */
export const STATE_RANK: Record<State, number> = { error: 0, warn: 1, sampled: 2, ok: 3, idle: 4 }

export function worst(states: State[]): State {
  return states.reduce<State>((w, s) => (STATE_RANK[s] < STATE_RANK[w] ? s : w), 'idle')
}

/** Glyph + word. `label` may be empty on narrow layouts, never the glyph. */
export function Status({ state, label, className }: { state: State; label: string; className?: string }) {
  return (
    <span className={cn('inline-flex items-center gap-1.5 whitespace-nowrap', className)}>
      <span aria-hidden className={cn('w-3 text-center', GLYPH_CLASS[state])}>{GLYPH[state]}</span>
      {label}
    </span>
  )
}

/** A link inside a StatusLine. Only for things the reader can act on. */
export function Phrase({ children, onClick, href }: { children: ReactNode; onClick?: () => void; href?: string }) {
  // Only a real action gets link styling and link semantics; a name with nothing behind it is text.
  if (!onClick && !href) return <span className="font-medium">{children}</span>
  return (
    <a href={href ?? '#'} onClick={(e) => { if (onClick) { e.preventDefault(); onClick() } }}>
      {children}
    </a>
  )
}

/**
 * The page's verdict. Inside the console shell it does not take a line of the
 * page: it renders into the header's attention slot as a glyph + count
 * (`× 2 · ◐ 1`), and the sentences show on demand when that is clicked. A
 * page with nothing wrong shows a quiet green glyph and no number. Outside a
 * shell (docs, demos) it renders inline as the line described below.
 *
 * Inline form: one glyph, one sentence, at most one link.
 *  - the glyph is the worst state on the page
 *  - the sentence is the single most important thing, under ~60 characters;
 *    it may wrap to a second line on a phone, it never truncates
 *  - the link (Phrase) is on the thing the reader can act on, if any
 *  - everything else lives in the page below; further problems collapse
 *    into `more` ("+1 warning"), a muted link on the right. Given `items`,
 *    it unfolds the line in place into one glyph + one sentence per item
 *    (each with its own optional link) and the label becomes "less".
 *    Given only `onClick`, it navigates instead
 * Counts, facts and "fine" things never appear here. If the page is fine,
 * the line says so in three or four words.
 * `sticky` pins it under the header while the page scrolls (default on).
 */
export type StatusItem = { state: State; children: ReactNode }

export function StatusLine(props: { state: State; children: ReactNode; more?: { label: string; onClick?: () => void; items?: StatusItem[] }; sticky?: boolean; className?: string }) {
  const slots = useShellSlots()
  if (slots?.attention) return createPortal(<AttentionEntry {...props} />, slots.attention)
  return <StatusLineInline {...props} />
}

/**
 * What one StatusLine contributes to the header: one row per sentence, each
 * tagged with its state. Several StatusLines on one screen (a page verdict
 * plus a tab's ledger verdict) all land in the same list, and the header's
 * AttentionHost counts them together instead of stacking two badges.
 */
function AttentionEntry({ state, children, more }: { state: State; children: ReactNode; more?: { label: string; onClick?: () => void; items?: StatusItem[] } }) {
  const all: StatusItem[] = [{ state, children }, ...(more?.items ?? [])]
  return (
    <>
      {all.map((it, i) => (
        <li key={i} data-state={it.state} className="flex items-baseline gap-3 py-0.5">
          <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[it.state])}>{GLYPH[it.state]}</span>
          <span className="min-w-0 flex-1">{it.children}</span>
        </li>
      ))}
      {more?.onClick && !more.items?.length && <li className="py-0.5 pl-6 text-xs"><a href="#" onClick={(e) => { e.preventDefault(); more.onClick?.() }}>{more.label}</a></li>}
    </>
  )
}

/**
 * The header's attention control. It owns the slot the StatusLines portal
 * into (`onSlot` hands it to ShellSlotsProvider), counts the rows by state,
 * and shows them on click. Quiet when nothing is wrong: one green glyph, no
 * number. Focus moves into the panel on open and back to the button on close.
 */
export function AttentionHost({ onSlot }: { onSlot: (el: HTMLElement | null) => void }) {
  const [open, setOpen] = useState(false)
  const [counts, setCounts] = useState({ errors: 0, warns: 0, total: 0 })
  const ref = useRef<HTMLDivElement>(null)
  const btn = useRef<HTMLButtonElement>(null)
  const list = useRef<HTMLUListElement | null>(null)
  const setList = (el: HTMLUListElement | null) => { list.current = el; onSlot(el) }
  useEffect(() => {
    const el = list.current; if (!el) return
    const count = () => {
      const states = [...el.querySelectorAll<HTMLElement>('li[data-state]')].map((li) => li.dataset.state as State)
      setCounts({ errors: states.filter((s) => s === 'error').length, warns: states.filter((s) => s === 'warn' || s === 'sampled').length, total: states.length })
    }
    count()
    const mo = new MutationObserver(count); mo.observe(el, { childList: true, subtree: true, attributes: true, attributeFilter: ['data-state'] })
    return () => mo.disconnect()
  }, [])
  useEffect(() => {
    if (!open) return
    const onDoc = (e: MouseEvent) => { if (!ref.current?.contains(e.target as Node)) setOpen(false) }
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') { setOpen(false); btn.current?.focus() } }
    document.addEventListener('mousedown', onDoc); document.addEventListener('keydown', onKey)
    const first = list.current?.querySelector<HTMLElement>('a, button'); (first ?? list.current)?.focus()
    return () => { document.removeEventListener('mousedown', onDoc); document.removeEventListener('keydown', onKey) }
  }, [open])
  const quiet = counts.errors === 0 && counts.warns === 0
  const label = quiet ? (counts.total ? 'nothing needs attention' : 'no verdict on this page') : `${counts.errors ? `${counts.errors} error${counts.errors > 1 ? 's' : ''}` : ''}${counts.errors && counts.warns ? ', ' : ''}${counts.warns ? `${counts.warns} warning${counts.warns > 1 ? 's' : ''}` : ''}`
  return (
    <div ref={ref} className="relative">
      <button ref={btn} type="button" onClick={() => setOpen((o) => !o)} aria-expanded={open} aria-haspopup="dialog" aria-label={label} title={label}
        className={cn('flex h-7 items-center gap-2 border px-2 font-mono text-[11px] tabular-nums hover:bg-muted', quiet && 'text-muted-foreground')}>
        {quiet ? <span aria-hidden className={counts.total ? GLYPH_CLASS.ok : GLYPH_CLASS.idle}>{counts.total ? GLYPH.ok : GLYPH.idle}</span> : (
          <>
            {counts.errors > 0 && <span className="flex items-center gap-1"><span aria-hidden className={GLYPH_CLASS.error}>{GLYPH.error}</span>{counts.errors}</span>}
            {counts.warns > 0 && <span className="flex items-center gap-1"><span aria-hidden className={GLYPH_CLASS.warn}>{GLYPH.warn}</span>{counts.warns}</span>}
          </>
        )}
      </button>
      {/* Always mounted: the StatusLines portal into the list, so it must exist while the panel is closed. */}
      <Drop anchor={ref} open width={480} role="dialog" label="attention" className={cn('op-status text-sm leading-6', !open && 'hidden')}>
        <div className="flex items-center justify-between border-b px-3 py-1.5"><span className="op-label">attention</span><span className="text-[11px] text-muted-foreground">{label}</span></div>
        <ul ref={setList} tabIndex={-1} className="px-3 py-2 outline-none">{counts.total === 0 && <li className="py-0.5 text-xs text-muted-foreground">this page has no verdict</li>}</ul>
      </Drop>
    </div>
  )
}

export function StatusLineInline({ state, children, more, sticky = true, className }: { state: State; children: ReactNode; more?: { label: string; onClick?: () => void; items?: StatusItem[] }; sticky?: boolean; className?: string }) {
  const [open, setOpen] = useState(false)
  const unfolds = !!more?.items?.length
  return (
    <div className={cn('op-status -mx-4 overflow-hidden border-b px-4 py-2 text-sm leading-6 sm:-mx-6 sm:px-6', sticky && 'op-sticky', className)}>
      <p className="flex items-baseline gap-3">
        <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[state])}>{GLYPH[state]}</span>
        <span className="min-w-0 flex-1 sm:truncate">{children}</span>
        {more && (
          <a href="#" aria-expanded={unfolds ? open : undefined} onClick={(e) => { e.preventDefault(); if (unfolds) setOpen((o) => !o); else more.onClick?.() }} className="shrink-0 text-xs text-muted-foreground">
            {unfolds && open ? 'less' : more.label}
          </a>
        )}
      </p>
      {unfolds && open && (
        <ul className="mt-2 space-y-1 border-t border-[var(--op-rule-soft)] pt-2">
          {more!.items!.map((it, i) => (
            <li key={i} className="flex items-baseline gap-3">
              <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[it.state])}>{GLYPH[it.state]}</span>
              <span className="min-w-0 flex-1 sm:truncate">{it.children}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
