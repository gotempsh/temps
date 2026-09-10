// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ExternalLink } from 'lucide-react'
import { useFeatureMaturity } from '@/hooks/useFeatureMaturity'
import { BETA_TOOLTIP, EXPERIMENTAL_TOOLTIP } from '@/lib/feature-maturity'
import { cn } from '@/lib/utils'
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'

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

  if (!feature || feature.maturity === 'stable') return null

  const experimental = feature.maturity === 'experimental'
  const label = experimental ? 'Experimental' : 'Beta'
  const promise = experimental ? EXPERIMENTAL_TOOLTIP : BETA_TOOLTIP

  return (
    <Tooltip delayDuration={150}>
      <TooltipTrigger asChild>
        <span
          className={cn(
            'inline-flex shrink-0 items-center rounded-full border font-medium leading-none transition-colors',
            compact ? 'px-1.5 py-0.5 text-[9px]' : 'px-2 py-1 text-[10px]',
            experimental
              ? 'border-amber-500/35 bg-amber-500/8 text-amber-700 dark:text-amber-300'
              : 'border-blue-500/30 bg-blue-500/8 text-blue-700 dark:text-blue-300',
            className
          )}
          aria-label={`${label} feature: ${feature.reason}`}
        >
          {label}
        </span>
      </TooltipTrigger>
      <TooltipContent className="w-80 space-y-2.5 p-3.5">
        <p className="text-xs font-semibold uppercase tracking-[0.14em] text-muted-foreground">
          {label} feature
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
          Learn what this means →
          <ExternalLink className="size-3" aria-hidden="true" />
        </a>
      </TooltipContent>
    </Tooltip>
  )
}
