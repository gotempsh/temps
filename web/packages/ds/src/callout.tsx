// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { AlertTriangle, CheckCircle2, Info, XCircle } from 'lucide-react'
import { cn } from './lib/cn'

export type CalloutTone = 'info' | 'success' | 'warning' | 'error'

const TONE_META: Record<CalloutTone, { icon: typeof Info; classes: string }> = {
  info: { icon: Info, classes: 'border-border bg-muted/40 text-foreground' },
  success: {
    icon: CheckCircle2,
    classes: 'border-success/30 bg-success/10 text-foreground',
  },
  warning: {
    icon: AlertTriangle,
    classes: 'border-warning/30 bg-warning/10 text-foreground',
  },
  error: {
    icon: XCircle,
    classes: 'border-destructive/30 bg-destructive/10 text-foreground',
  },
}

export interface CalloutProps {
  tone?: CalloutTone
  title?: ReactNode
  children: ReactNode
  className?: string
}

/** A tone-only inline notice — for in-page banners, not toasts. See RULES.md § Notifications. */
export function Callout({ tone = 'info', title, children, className }: CalloutProps) {
  const meta = TONE_META[tone]
  const Icon = meta.icon
  return (
    <div
      role={tone === 'error' ? 'alert' : 'status'}
      className={cn('flex gap-3 rounded-md border p-3 text-sm', meta.classes, className)}
    >
      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden />
      <div className="space-y-1">
        {title ? <p className="font-medium">{title}</p> : null}
        <div className="text-muted-foreground">{children}</div>
      </div>
    </div>
  )
}
