// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext, useMemo, type ReactNode } from 'react'
import { useDocToc } from '@/components/shell-context'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   Shared scaffolding for the design-system reference pages (Brand,
   Foundations, Components, Page patterns, Kitchen sink, Operator
   components). Every reference page renders under the v1 skin so the
   documentation looks like the thing it documents — the skin, the rails and
   the column belong to the shell (`src/components/Layout.tsx`).

   DocPage   the page's own header (eyebrow + intro), its blocks, and the
             list it hands the shell's "on this page" rail
   Block     one topic: title, rule (prose), optional api (mono pre), demos
   Demo      labelled example inside a Block
   Rule      short do / don't callout (state = ok | error)
   OneDemo   render exactly one Block's demos out of a whole blocks component
   ──────────────────────────────────────────────────────────────────────── */

/**
 * The blocks components in `src/sections/blocks/` are written as one flat run
 * of `Block`s, which is right for `/op-components` — the gallery is the run.
 * `/guide` needs the other cut: one document's `##` subsection on the left and
 * *that subsection's* demo on the right. Rather than fork the demos into a
 * second copy that would drift, `OneDemo` renders the whole blocks component
 * with a filter in context: every `Block` whose id does not match returns
 * null, and the matching one drops its own heading, rule and api (the prose
 * column beside it already says all three) and emits just its demos.
 *
 * The consequence to keep in mind: the component still runs whole, so its
 * hooks and fixtures still execute. That is deliberate — a `Block` cannot be
 * hoisted out of its component without hoisting its state with it.
 */
type BlockFilter = { id: string; demosOnly: boolean }
const BlockFilterCtx = createContext<BlockFilter | null>(null)

export function OneDemo({ id, children }: { id: string; children: ReactNode }) {
  const filter = useMemo(() => ({ id, demosOnly: true }), [id])
  return <BlockFilterCtx.Provider value={filter}>{children}</BlockFilterCtx.Provider>
}

/**
 * For the two blocks files that keep their own local `Block` (they predate
 * this one and have their own demo chrome): call this at the top of it and
 * obey the two verdicts, so `OneDemo` works there too.
 */
export function useBlockFilter(id: string): 'hide' | 'demos' | 'whole' {
  const filter = useContext(BlockFilterCtx)
  if (!filter) return 'whole'
  if (filter.id !== id) return 'hide'
  return filter.demosOnly ? 'demos' : 'whole'
}

export function DocPage({ eyebrow, intro, toc, children }: { eyebrow: string; intro: ReactNode; toc: readonly (readonly [string, string])[]; children: ReactNode }) {
  // The right rail lives in the shell; the page only says what goes in it.
  useDocToc(useMemo(() => toc.map(([id, text]) => ({ id, text })), [toc]))
  return (
    <>
      <div className="text-xs">
        <p className="op-label">{eyebrow}</p>
        <p className="op-prose mt-1 max-w-[72ch] text-sm text-muted-foreground">{intro}</p>
      </div>
      <div className="mt-6 min-w-0 space-y-12">{children}</div>
    </>
  )
}

export function Block({ id, title, rule, api, children }: { id: string; title: string; rule: ReactNode; api?: string; children: ReactNode }) {
  const filter = useContext(BlockFilterCtx)
  if (filter) {
    if (filter.id !== id) return null
    // No section id here: `/guide` already owns the heading and its anchor, and
    // two elements answering to `#viz-band` is how a "link to this" stops working.
    if (filter.demosOnly) return <div className="min-w-0 space-y-4">{children}</div>
  }
  return (
    <section id={id} className="scroll-mt-16 border-t pt-8">
      <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
        <div className="min-w-0">
          <h2 className="op-h2">{title}</h2>
          <div className="op-prose mt-2 space-y-2 text-sm text-muted-foreground">{rule}</div>
          {/* Focusable: a scrollable region a keyboard cannot reach is a serious
              axe violation, and these panes scroll at narrow widths. */}
          {api && (
            <pre
              tabIndex={0}
              className="op-inset mt-4 overflow-auto border p-3 font-mono text-[11px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
            >
              {api}
            </pre>
          )}
        </div>
        <div className="min-w-0 space-y-4">{children}</div>
      </div>
    </section>
  )
}

export function Demo({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  return (
    <div className="min-w-0">
      <p className="op-label mb-2">{label}</p>
      <div className={cn('min-w-0 px-4 sm:px-6', className)}>{children}</div>
    </div>
  )
}

/** A verdict on a practice. `state="ok"` is the rule, `state="error"` the thing it replaces. */
export function Rule({ state, children }: { state: 'ok' | 'error'; children: ReactNode }) {
  return (
    <p className="flex items-start gap-2 text-sm">
      <span aria-hidden className={cn('w-3 shrink-0 text-center', state === 'ok' ? 'text-success' : 'text-destructive')}>{state === 'ok' ? '●' : '×'}</span>
      <span className="min-w-0">{children}</span>
    </p>
  )
}
