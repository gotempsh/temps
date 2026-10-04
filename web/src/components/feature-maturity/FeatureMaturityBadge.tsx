// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type KeyboardEvent, type MouseEvent } from 'react'
import { ExternalLink, FlaskConical, TestTubeDiagonal } from 'lucide-react'
import { useFeatureMaturity } from '@/hooks/useFeatureMaturity'
import { BETA_TOOLTIP, EXPERIMENTAL_TOOLTIP } from '@/lib/feature-maturity'
import { cn } from '@/lib/utils'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'

/**
 * Icon-only maturity marker. The explanation lives in a popover rather than a
 * tooltip because it carries a docs link, which a hover tooltip cannot keep
 * reachable.
 *
 * Most call sites render this inside a navigation `<Link>` or a tab trigger, so
 * the trigger is a focusable span (a `<button>` inside `<a>` is invalid) and
 * every click is stopped before it reaches that parent. The content is
 * portalled, but React still bubbles its events through the component tree, so
 * it stops propagation too — otherwise following the docs link would also
 * navigate the surrounding link.
 */
export function FeatureMaturityBadge({
  featureKey,
  compact = false,
  className,
}: {
  featureKey?: string
  compact?: boolean
  className?: string
}) {
  const { feature } = useFeatureMaturity(featureKey)
  const [open, setOpen] = useState(false)

  if (!feature || feature.maturity === 'stable') return null

  const experimental = feature.maturity === 'experimental'
  const label = experimental ? 'Experimental' : 'Beta'
  const promise = experimental ? EXPERIMENTAL_TOOLTIP : BETA_TOOLTIP
  const Icon = experimental ? FlaskConical : TestTubeDiagonal
  const tone = experimental
    ? 'text-amber-600 dark:text-amber-400'
    : 'text-blue-600 dark:text-blue-400'

  const toggle = (event: MouseEvent | KeyboardEvent) => {
    event.preventDefault()
    event.stopPropagation()
    setOpen((current) => !current)
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <span
          role="button"
          tabIndex={0}
          aria-label={`${label} feature. Show details`}
          onClick={toggle}
          onKeyDown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') toggle(event)
          }}
          className={cn(
            'inline-flex shrink-0 cursor-help items-center justify-center rounded-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
            compact ? 'size-4' : 'size-5',
            tone,
            experimental ? 'hover:bg-amber-500/10' : 'hover:bg-blue-500/10',
            className
          )}
        >
          <Icon
            className={compact ? 'size-3' : 'size-3.5'}
            aria-hidden="true"
          />
        </span>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-80 space-y-2 p-3.5"
        onClick={(event) => event.stopPropagation()}
      >
        <p
          className={cn(
            'flex items-center gap-1.5 text-xs font-semibold',
            tone
          )}
        >
          <Icon className="size-3.5" aria-hidden="true" />
          {label}
        </p>
        <p className="text-sm leading-relaxed">{promise}</p>
        <p className="text-xs leading-relaxed text-muted-foreground">
          {feature.reason}
        </p>
        <a
          href={feature.docs_path}
          target="_blank"
          rel="noreferrer"
          className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:underline"
        >
          Learn what this means
          <ExternalLink className="size-3" aria-hidden="true" />
        </a>
      </PopoverContent>
    </Popover>
  )
}
