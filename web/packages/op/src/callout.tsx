// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from './lib/cn'
import { GLYPH, GLYPH_CLASS, type State } from './status'

/**
 * An alert that lives inside a page. A StatusLine is one sentence and rolls
 * up into the header; a Callout is for a fault that needs its evidence shown
 * where it applies: the glyph and the title in the state colour, a 2px rule
 * on the left in the same colour and no box (the rule is the alert; a frame
 * around it is a frame inside the page's frames), the raw message from the
 * other system in mono on the inset tone (quoted, never paraphrased), one
 * sentence of consequence and what the action changes, and the action. Error is red, warn amber, ok green,
 * idle ink. Never used for decoration: if nothing is wrong, nothing shows.
 */
const RULE: Record<State, string> = { error: 'border-l-destructive', warn: 'border-l-warning', ok: 'border-l-success', idle: 'border-l-foreground', sampled: 'border-l-muted-foreground' }
export function Callout({ state, title, quote, action, children, className }: { state: State; title: ReactNode; /** What the other system said, verbatim. */ quote?: ReactNode; action?: ReactNode; children?: ReactNode; className?: string }) {
  return (
    <div role={state === 'error' ? 'alert' : 'status'} className={cn('border-l-2 py-1 pl-4 text-xs', RULE[state], className)}>
      <div className="flex min-w-0 flex-wrap items-start gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1 space-y-1.5">
          <p className={cn('flex items-baseline gap-2 text-sm font-semibold leading-5', GLYPH_CLASS[state])}><span aria-hidden className="w-3 text-center">{GLYPH[state]}</span><span className="min-w-0">{title}</span></p>
          {quote && <p className="op-inset px-3 py-1.5 font-mono text-[11px] leading-5 text-foreground">{quote}</p>}
          {children && <p className="text-muted-foreground">{children}</p>}
        </div>
        {action && <div className="flex shrink-0 items-center gap-2 sm:pt-0.5">{action}</div>}
      </div>
    </div>
  )
}
