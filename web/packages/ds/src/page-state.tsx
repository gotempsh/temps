// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import type { LucideIcon } from 'lucide-react'
import { Button } from '@temps-sdk/ui'
import { cn } from './lib/cn'

/**
 * Consolidates `EmptyPlaceholder` and `EmptyState`
 * (web/src/components/ui/empty-placeholder.tsx, empty-state.tsx — 101 lines
 * combined, near-duplicates) into one primitive covering every reason a page
 * has nothing to show:
 *
 * - `empty`: the resource exists and is configured, there's just nothing in
 *   it yet ("No deployments yet").
 * - `not-set-up`: CLAUDE.md's rule, made a first-class variant instead of a
 *   nice-to-have. A feature that depends on optional operator config (AI
 *   provider, S3 bucket, SMTP, DNS token) must never render nothing —
 *   `requirement` and `example` are REQUIRED for this variant so the
 *   surface always says what's missing and what it would do, and `action`
 *   should link straight to the settings page that configures it.
 * - `failed`: a request errored. Pairs with a retry action.
 */
export type PageStateVariant = 'empty' | 'not-set-up' | 'failed'

interface PageStateBaseProps {
  icon: LucideIcon
  title: string
  description?: ReactNode
  action?: ReactNode
  size?: 'default' | 'compact'
  className?: string
}

interface EmptyPageStateProps extends PageStateBaseProps {
  variant: 'empty' | 'failed'
}

interface NotSetUpPageStateProps extends PageStateBaseProps {
  variant: 'not-set-up'
  /** What's missing, in plain words — "No AI provider configured." */
  requirement: string
  /** A concrete example of what the feature would do once configured. */
  example: ReactNode
  /** Link straight to the settings page that configures it. */
  settingsHref: string
  settingsLabel?: string
}

export type PageStateProps = EmptyPageStateProps | NotSetUpPageStateProps

export function PageState(props: PageStateProps) {
  const { icon: Icon, title, size = 'default', className } = props
  const compact = size === 'compact'

  return (
    <div
      data-page-state={props.variant}
      className={cn(
        'flex flex-col items-center justify-center rounded-lg text-center animate-in fade-in-50',
        compact ? 'min-h-60 gap-3 p-6' : 'min-h-[25rem] gap-4 p-8',
        className,
      )}
    >
      <div
        className={cn(
          'flex items-center justify-center rounded-full bg-muted',
          compact ? 'size-14' : 'size-20',
        )}
      >
        <Icon className={cn('text-muted-foreground', compact ? 'size-7' : 'size-10')} />
      </div>
      <div className="max-w-md space-y-2">
        <h3 className={cn('font-semibold', compact ? 'text-base' : 'text-lg')}>{title}</h3>
        {props.variant === 'not-set-up' ? (
          <div className="space-y-2 text-left sm:text-center">
            <p className="text-sm text-muted-foreground">{props.requirement}</p>
            <p className="rounded-md border border-dashed bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
              <span className="font-medium text-foreground">Example: </span>
              {props.example}
            </p>
          </div>
        ) : props.description ? (
          <p className="text-sm text-muted-foreground">{props.description}</p>
        ) : null}
      </div>
      {props.variant === 'not-set-up' ? (
        <Button asChild>
          <a href={props.settingsHref}>{props.settingsLabel ?? 'Open settings'}</a>
        </Button>
      ) : (
        props.action
      )}
    </div>
  )
}
