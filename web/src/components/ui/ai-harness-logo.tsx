// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Bot } from 'lucide-react'
import { cn } from '@/lib/utils'
import { canonicalHarnessId } from './ai-harness-brand'

/**
 * `src` is an official brand asset bundled under `public/ai-harnesses`. A
 * harness without one gets a neutral text `monogram` instead; never draw or
 * approximate a third-party mark.
 */
type HarnessBrand =
  | { label: string; src: string; monogram?: never }
  | { label: string; monogram: string; src?: never }

const HARNESS_BRANDS: Record<string, HarnessBrand> = {
  claude_cli: {
    label: 'Claude Code',
    src: '/ai-harnesses/claude-code.svg',
  },
  codex_cli: {
    label: 'Codex',
    src: '/ai-harnesses/codex.svg',
  },
  opencode: {
    label: 'OpenCode',
    src: '/ai-harnesses/opencode.svg',
  },
  pi: {
    label: 'pi',
    monogram: 'π',
  },
}

export function AiHarnessLogo({
  providerId,
  size = 24,
  className,
}: {
  providerId: string
  size?: number
  className?: string
}) {
  const canonicalId = canonicalHarnessId(providerId)
  const brand = HARNESS_BRANDS[canonicalId]

  return (
    <span
      aria-label={`${brand?.label ?? providerId} logo`}
      data-harness={canonicalId}
      role="img"
      className={cn(
        'inline-flex shrink-0 items-center justify-center',
        className
      )}
      style={{ height: size, width: size }}
    >
      <HarnessMark brand={brand} size={size} />
    </span>
  )
}

function HarnessMark({
  brand,
  size,
}: {
  brand: HarnessBrand | undefined
  size: number
}) {
  if (brand?.src) {
    return <img className="size-full object-contain" src={brand.src} alt="" />
  }
  if (brand?.monogram) {
    return (
      <span
        aria-hidden="true"
        className="font-semibold leading-none text-foreground"
        style={{ fontSize: Math.round(size * 0.9) }}
      >
        {brand.monogram}
      </span>
    )
  }
  return <Bot className="size-[62%] text-muted-foreground" />
}
