// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import {
  Ban,
  CircleCheck,
  CornerUpRight,
  ScrollText,
  ShieldQuestionMark,
  type LucideIcon,
} from 'lucide-react'
import type { RuleAction } from './types'

const ACTION_ICONS: Record<RuleAction['type'], LucideIcon> = {
  block: Ban,
  allow: CircleCheck,
  challenge: ShieldQuestionMark,
  redirect: CornerUpRight,
  log: ScrollText,
}

interface ActionIconProps {
  type: RuleAction['type']
  className?: string
}

/** Decorative: every use sits next to the action's text label. */
export function ActionIcon({ type, className }: ActionIconProps) {
  const Icon = ACTION_ICONS[type]
  return (
    <Icon className={cn('size-4 shrink-0', className)} aria-hidden="true" />
  )
}
