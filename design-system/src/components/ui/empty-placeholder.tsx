// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { LucideIcon } from 'lucide-react'
import { cn } from '@/lib/utils'

interface EmptyStep {
  label: string
  href: string
  done?: boolean
}

interface EmptyPlaceholderProps extends React.HTMLAttributes<HTMLDivElement> {
  icon?: LucideIcon
  title: string
  description: string
  action?: React.ReactNode
  /**
   * Operator-console onboarding treatment (temps/CLAUDE.md: unconfigured
   * features must onboard, never disappear). `preview` is a dimmed mock of
   * what the surface looks like once configured; `steps` is the checklist of
   * what's missing, each linking to where it gets configured. When either is
   * passed the layout switches to two columns; otherwise the stock centred
   * icon-disc layout is unchanged for every existing consumer.
   */
  preview?: React.ReactNode
  steps?: EmptyStep[]
}

export function EmptyPlaceholder({
  icon: Icon,
  title,
  description,
  action,
  preview,
  steps,
  className,
  ...props
}: EmptyPlaceholderProps) {
  if (preview || steps) {
    return (
      <div
        className={cn(
          'grid gap-px overflow-hidden border bg-border md:grid-cols-[3fr_2fr]',
          className
        )}
        {...props}
      >
        <div
          aria-hidden
          className="pointer-events-none relative select-none overflow-hidden bg-background p-3 opacity-60"
        >
          {preview}
          <div className="absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-background to-transparent" />
        </div>
        <div className="flex flex-col gap-3 bg-background p-4">
          <div className="flex items-center gap-2">
            {Icon && <Icon className="h-4 w-4 text-muted-foreground" />}
            <h3 className="text-sm font-semibold">{title}</h3>
          </div>
          <p className="op-prose text-sm text-muted-foreground">{description}</p>
          {steps && steps.length > 0 && (
            <ol className="space-y-1 text-xs">
              {steps.map((s) => (
                <li key={s.label} className="flex items-start gap-2">
                  <span
                    className={cn(
                      'shrink-0 tabular-nums',
                      s.done ? 'text-success' : 'text-muted-foreground'
                    )}
                  >
                    [{s.done ? 'x' : ' '}]
                  </span>
                  <a
                    href={s.href}
                    className={cn(
                      'underline-offset-4 hover:underline focus-visible:outline-2 focus-visible:outline-ring',
                      s.done && 'text-muted-foreground line-through'
                    )}
                  >
                    {s.label}
                  </a>
                </li>
              ))}
            </ol>
          )}
          {action && <div className="mt-auto pt-2">{action}</div>}
        </div>
      </div>
    )
  }

  return (
    <div
      className={cn(
        'flex min-h-[400px] flex-col items-center justify-center rounded-md p-8 text-center animate-in fade-in-50',
        className
      )}
      {...props}
    >
      {Icon && (
        <div className="flex h-20 w-20 items-center justify-center rounded-full bg-muted">
          <Icon className="h-10 w-10" />
        </div>
      )}
      <h3 className="mt-4 text-lg font-semibold">{title}</h3>
      <p className="mt-2 mb-4 text-sm text-muted-foreground">{description}</p>
      {action}
    </div>
  )
}
export type { EmptyStep }
