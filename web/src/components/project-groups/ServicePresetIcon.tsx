// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PackageOpen } from 'lucide-react'
import { projectPresetLabel } from '@/components/dashboard/project-card-data'
import { PresetIcon } from '@/components/presets/PresetIcon'
import { cn } from '@/lib/utils'

/** A service's preset logo, or a neutral box when it has no preset. */
export function ServicePresetIcon({ preset, className }: { preset?: string | null; className?: string }) {
  if (!preset) {
    return (
      <span
        className={cn(
          'flex shrink-0 items-center justify-center rounded-md border bg-background text-muted-foreground',
          className
        )}
      >
        <PackageOpen className="size-3" />
      </span>
    )
  }
  return (
    <PresetIcon
      preset={preset}
      label={projectPresetLabel(preset)}
      className={cn('shrink-0 rounded-md', className)}
      imageClassName="p-0.5"
    />
  )
}
