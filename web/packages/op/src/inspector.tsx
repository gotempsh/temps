// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useRef, type ReactNode } from 'react'
import { X } from 'lucide-react'
import { cn } from './lib/cn'
import { Kbd } from './kbd'
import { Status, type State } from './status'

/**
 * The row read beside the list. A tool screen (logs, proxy access, the audit
 * log) is one long list the reader narrows until it answers a question, and
 * the answer is usually one row: opening it as a page loses the list, the
 * cursor and the query, and the reader has to walk back for the next row.
 * The Inspector is that row's detail *beside* the list — the record page
 * stays as the deep link, and `open` in the header is how you get to it.
 *
 * Three widths, one component, because the shape is a consequence of how
 * much room there is and not of what the screen wants:
 * - from xl it is `sticky` and therefore **in flow**: the main column is
 *   pushed to make room for it, never covered. A tool screen's list is the
 *   context; hiding it under a sheet is the thing this component exists to
 *   avoid. 520px is the width it wants, capped at 45% of what it was given,
 *   because a panel that leaves the list narrower than itself has stopped
 *   being a panel beside a list. It sticks to `--op-shell-top`, which a console shell sets to the
 *   bottom edge of its own sticky header (the offset plus the header's
 *   height); outside a shell it falls back to a 1rem gutter. The panel then
 *   scrolls its own body while the list scrolls behind it, and neither
 *   header moves.
 * - between md and xl there is no room to push, so it is a right-hand sheet
 *   over a scrim, and the scrim closes it.
 * - below md a 520px sheet is the whole screen anyway, so it is full-screen
 *   and says so.
 * The skin is ink: one border on the left edge, no card, no shadow, no
 * rounding — it is a division of the page, not an object floating over it.
 *
 * The component owns the chrome and the keyboard/focus contract; **the
 * content is a child**, so the screen keeps owning what a row means:
 *
 * - `⏎` on a ledger row opens it (the screen's `onOpen`).
 * - `j`/`k` keep moving the *ledger's* cursor while it is open, and the
 *   screen re-points the panel at whatever the cursor lands on. That is why
 *   opening must not move focus: the panel follows the list, the list does
 *   not follow the panel.
 * - `esc` closes and puts focus back on the row it came from (`returnFocus`).
 * - `/` belongs to the page's query bar at all times and is never read here.
 * - Focus enters the panel with `Tab`, and only with `Tab`.
 * - `1`–`9` jump to the body's anchors, which is why the body is stacked
 *   `Section`s behind a small toc rather than tabs: a tab hides two thirds
 *   of a record, and a reader who arrived with a question does not know
 *   which third to look in.
 *
 * `role="complementary"` with a name, and the title is `aria-live="polite"`
 * so that a panel following the cursor announces what it is now showing
 * instead of changing silently behind the reader.
 */
export type InspectorAnchor = { id: string; label: string }

export function Inspector({
  open, label, state, word, title, meta,
  anchors = [], onOpen, openLabel = 'open', onCopyLink, onClose, returnFocus,
  children, className,
}: {
  open: boolean
  /** Names the region for a screen reader, e.g. "log line inspector". */
  label: string
  /** The state of the thing being inspected; drawn as glyph + word, never a bare dot. */
  state: State
  /** The state word beside the glyph ("error", "warn", "info"). */
  word: string
  /** The mono identifier this panel is about. Announced politely as the cursor moves. */
  title: ReactNode
  /** When it happened and which deployment was live: the two facts a title never has room for. */
  meta?: ReactNode
  /** Ids of the body's sections, in order, reachable as `1`–`9`. */
  anchors?: InspectorAnchor[]
  /** Go to the full record page. A panel is a reading of a record, not a replacement for it. */
  onOpen?: () => void
  openLabel?: string
  onCopyLink?: () => void
  onClose: () => void
  /** The row to hand focus back to on `esc`; re-read on close, because the cursor may have moved. */
  returnFocus?: () => HTMLElement | null | undefined
  children: ReactNode
  className?: string
}) {
  const bodyRef = useRef<HTMLDivElement>(null)

  const close = useCallback(() => {
    onClose()
    // Focus goes back to the row, not to the body: `esc` is a return, and a
    // reader who lands on <body> has to Tab through the page to reach the list again.
    returnFocus?.()?.focus()
  }, [onClose, returnFocus])

  const jump = useCallback((id: string) => {
    const el = bodyRef.current?.querySelector<HTMLElement>(`#${CSS.escape(id)}`)
    if (!el) return
    el.scrollIntoView({ block: 'start' })
    // The heading is not focusable on its own; make it so for this jump, so a
    // keyboard reader's next Tab continues from the section they asked for.
    el.tabIndex = -1
    el.focus({ preventScroll: true })
  }, [])

  useEffect(() => {
    if (!open) return
    const onKey = (e: globalThis.KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || e.metaKey || e.ctrlKey) return
      if (e.key === 'Escape') { e.preventDefault(); close(); return }
      const n = Number(e.key)
      if (Number.isInteger(n) && n >= 1 && n <= anchors.length) { e.preventDefault(); jump(anchors[n - 1].id) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open, anchors, close, jump])

  if (!open) return null
  return (
    <>
      {/* Below xl the panel covers the list, so it needs a way out that is not the keyboard. */}
      <button type="button" aria-label="close the inspector" onClick={close} className="fixed inset-0 z-30 bg-foreground/20 xl:hidden" />
      <aside
        role="complementary" aria-label={label}
        className={cn(
          'fixed bottom-0 left-0 right-0 top-0 z-40 flex min-w-0 flex-col border-l bg-background',
          'md:left-auto md:w-[min(520px,100vw_-_1.5rem)]',
          'xl:sticky xl:bottom-auto xl:left-auto xl:right-auto xl:top-[var(--op-shell-top,1rem)] xl:z-auto xl:h-[calc(100vh_-_var(--op-shell-top,1rem))] xl:w-[520px] xl:max-w-[45%] xl:shrink-0',
          className,
        )}
      >
        <div className="flex min-w-0 shrink-0 items-center gap-2 border-b px-3 py-2 text-xs">
          <Status state={state} label={word} className="shrink-0" />
          <span aria-live="polite" className="min-w-0 truncate font-mono">{title}</span>
          {meta && <span className="hidden min-w-0 shrink-[2] truncate font-mono text-[11px] text-muted-foreground sm:block">{meta}</span>}
          <span className="ms-auto flex shrink-0 items-center gap-1">
            {onOpen && <button type="button" onClick={onOpen} className="inline-flex h-6 items-center border px-2 text-[11px] hover:bg-muted">{openLabel}</button>}
            {onCopyLink && <button type="button" onClick={onCopyLink} className="inline-flex h-6 items-center border px-2 text-[11px] hover:bg-muted">copy link</button>}
            <button type="button" onClick={close} aria-label="close the inspector" className="inline-flex h-6 w-6 items-center justify-center text-muted-foreground hover:text-foreground">
              <X aria-hidden className="h-3.5 w-3.5" />
            </button>
          </span>
        </div>
        {anchors.length > 0 && (
          <nav aria-label="sections of this record" className="flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-b px-3 py-1.5 text-[11px] text-muted-foreground">
            {anchors.map((a, i) => (
              <button key={a.id} type="button" onClick={() => jump(a.id)} className="inline-flex items-center gap-1 hover:text-foreground">
                <span className="font-mono">{a.label}</span>
                <Kbd keys={String(i + 1)} />
              </button>
            ))}
            <span className="ms-auto flex items-center gap-1"><Kbd keys="esc" /> close</span>
          </nav>
        )}
        <div ref={bodyRef} className="min-h-0 flex-1 overflow-y-auto px-3 py-2">{children}</div>
      </aside>
    </>
  )
}
