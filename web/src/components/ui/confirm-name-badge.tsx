// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { CopyButton } from '@/components/ui/copy-button'

interface ConfirmNameBadgeProps {
  value: string
}

/**
 * Renders a resource name as a clickable chip for "type X to confirm"
 * destructive-action prompts, so the user can copy it instead of retyping
 * it by hand. Rectangular rather than the pill-shaped `Badge`, and dashed,
 * so it reads as a copy affordance rather than a status tag.
 */
export function ConfirmNameBadge({ value }: ConfirmNameBadgeProps) {
  return (
    <CopyButton
      value={value}
      label={`Copy "${value}"`}
      className="mx-1 inline-flex items-center gap-1.5 rounded-md border border-dashed border-input bg-muted/40 px-2 py-0.5 align-middle font-mono font-semibold text-foreground hover:border-solid"
    >
      {value}
    </CopyButton>
  )
}
