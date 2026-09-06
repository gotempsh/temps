// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   Shared scaffolding for the design-system reference pages (Brand,
   Foundations, Components, Page patterns, Kitchen sink, Operator
   components). Every reference page renders under the v1 skin so the
   documentation looks like the thing it documents.

   DocPage   skin wrapper · header strip (eyebrow + intro) · content · TOC
   Block     one topic: title, rule (prose), optional api (mono pre), demos
   Demo      labelled example inside a Block
   Rule      short do / don't callout (state = ok | error)
   ──────────────────────────────────────────────────────────────────────── */

export function DocPage({ eyebrow, intro, toc, children }: { eyebrow: string; intro: ReactNode; toc: readonly (readonly [string, string])[]; children: ReactNode }) {
  return (
    <div className="operator ink v1 -m-4 sm:-m-6 lg:-m-8">
      <div className="border-b px-4 py-3 text-xs sm:px-6">
        <p className="op-label">{eyebrow}</p>
        <p className="op-prose mt-1 max-w-3xl text-sm text-muted-foreground">{intro}</p>
      </div>
      <div className="flex gap-10 px-4 py-6 sm:px-6">
        <div className="min-w-0 flex-1 space-y-12">{children}</div>
        <nav className="sticky top-6 hidden h-fit w-44 shrink-0 text-xs xl:block">
          <p className="op-label mb-2">on this page</p>
          {toc.map(([id, l]) => <a key={id} href={`#${id}`} className="block py-1 text-muted-foreground hover:text-foreground">{l}</a>)}
        </nav>
      </div>
    </div>
  )
}

export function Block({ id, title, rule, api, children }: { id: string; title: string; rule: ReactNode; api?: string; children: ReactNode }) {
  return (
    <section id={id} className="scroll-mt-6 border-t pt-8">
      <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
        <div className="min-w-0">
          <h2 className="op-h2">{title}</h2>
          <div className="op-prose mt-2 space-y-2 text-sm text-muted-foreground">{rule}</div>
          {api && <pre className="op-inset mt-4 overflow-auto border p-3 font-mono text-[11px] leading-5">{api}</pre>}
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
