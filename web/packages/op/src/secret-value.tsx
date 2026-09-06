// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Eye, EyeOff } from 'lucide-react'
import { CopyButton } from './ui/copy-button'
import { cn } from './lib/cn'

/**
 * A variable value in a row. Plain values show as mono text with a copy
 * button. Secrets show as dots until revealed: the eye toggles the value,
 * the copy button always copies the real value, so a secret can be pasted
 * without ever being shown on screen. Revealing is per row; a page-level
 * "show values" is the caller's `revealed` override. In the real console
 * a reveal is an API call that is audit-logged; keep the toggle so the log
 * has something to record.
 */
export function SecretValue({ value, secret, revealed = false, onToggle, className }: {
  value: string
  secret: boolean
  revealed?: boolean
  onToggle?: () => void
  className?: string
}) {
  const shown = !secret || revealed
  return (
    <span className={cn('flex min-w-0 items-center gap-1 font-mono', className)}>
      <span className={cn('min-w-0 truncate', !shown && 'tracking-wider text-muted-foreground')} aria-label={shown ? undefined : 'hidden secret value'}>{shown ? value : '••••••••••••'}</span>
      {secret && (
        <button type="button" onClick={onToggle} aria-pressed={revealed} aria-label={revealed ? 'Hide value' : 'Reveal value'} title={revealed ? 'hide' : 'reveal'} className="inline-flex h-7 w-7 shrink-0 items-center justify-center text-muted-foreground hover:text-foreground">
          {revealed ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
        </button>
      )}
      <CopyButton value={value} minimal label="Copy value" className="h-7 w-7 shrink-0 text-muted-foreground hover:text-foreground" />
    </span>
  )
}
