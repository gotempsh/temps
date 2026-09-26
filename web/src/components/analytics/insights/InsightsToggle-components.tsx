// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Lightbulb } from 'lucide-react'
import { type InsightsToggleButtonProps } from './InsightsToggle-shared'

/** Compact icon button that shows or hides a page's insights panel. */
export function InsightsToggleButton({
  open,
  onToggle,
}: InsightsToggleButtonProps) {
  const label = open ? 'Hide insights' : 'Show insights'
  return (
    <Button
      variant={open ? 'secondary' : 'outline'}
      size="sm"
      aria-pressed={open}
      title={label}
      onClick={() => onToggle(!open)}
    >
      <Lightbulb className="size-4 shrink-0" />
      <span className="sr-only">{label}</span>
    </Button>
  )
}
