// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'

/**
 * One on/off setting as a flat row: what it does on the left, the switch on
 * the right. Rows sit in a `divide-y` list inside a settings group, never in a
 * box of their own. A switch takes effect when flipped, so there is no Save.
 */
export function SettingToggleRow({
  id,
  label,
  description,
  checked,
  onCheckedChange,
  disabled = false,
}: {
  id: string
  label: string
  description?: ReactNode
  checked: boolean
  onCheckedChange: (checked: boolean) => void
  disabled?: boolean
}) {
  return (
    // `setting-<id>` lets a link (and the command palette) land on this row.
    <div
      id={`setting-${id}`}
      className="flex scroll-mt-4 items-start justify-between gap-6 py-4 first:pt-0 last:pb-0"
    >
      <div className="min-w-0 space-y-1">
        <Label htmlFor={id} className="text-sm font-medium">
          {label}
        </Label>
        {description && (
          <p className="text-sm text-muted-foreground">{description}</p>
        )}
      </div>
      <Switch
        id={id}
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
        className="mt-0.5 shrink-0"
      />
    </div>
  )
}
