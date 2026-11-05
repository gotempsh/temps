// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Lightbulb } from 'lucide-react'
import { type InsightsToggleButtonProps } from './InsightsToggle-shared'

/**
 * Shows or hides a page's insights panel: a compact icon button, or with
 * `showLabel` one that says what it does, for a page with no toolbar around it.
 */
export function InsightsToggleButton({
  open,
  onToggle,
  showLabel = false,
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
      <span className={showLabel ? undefined : 'sr-only'}>{label}</span>
    </Button>
  )
}
