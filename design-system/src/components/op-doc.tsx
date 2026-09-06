// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, type ReactNode } from 'react'
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
   ──────────────────────────────────────────────────────────────────────── */

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
