// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Bot } from 'lucide-react'
import { cn } from '@/lib/utils'
import { canonicalHarnessId } from './ai-harness-brand'

type HarnessBrand = {
  label: string
  src: string
}

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
      {brand ? (
        <img className="size-full object-contain" src={brand.src} alt="" />
      ) : (
        <Bot className="size-[62%] text-muted-foreground" />
      )}
    </span>
  )
}
