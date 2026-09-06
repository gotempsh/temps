// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from './lib/cn'

/** Platform-aware modifier. Pass '⌘' in `keys` and it becomes Ctrl off macOS. */
export const IS_MAC = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.userAgent)
export const MOD = IS_MAC ? '⌘' : 'Ctrl'

/**
 * Key badge. Sits inside primary buttons ("deploy ⌘⏎"), in footers
 * ("j k move"), and next to inputs ("/"). Never the only way to reach an
 * action: a badge is an accelerator, the button is the entry point.
 */
export function Kbd({ keys, className }: { keys: string | string[]; className?: string }) {
  const arr = (Array.isArray(keys) ? keys : [keys]).map((k) => (k === '⌘' ? MOD : k))
  return (
    <span className={cn('inline-flex items-center gap-0.5', className)}>
      {arr.map((k, i) => (
        <kbd key={i} className="inline-flex h-4 min-w-4 items-center justify-center border px-1 font-mono text-[10px] leading-none">{k}</kbd>
      ))}
    </span>
  )
}
