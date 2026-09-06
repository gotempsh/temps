// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'

/**
 * The real mark, copied from temps-landing/public/{favicon.svg,logo/*} —
 * not invented here. Two SVGs exist for a reason: `temps-icon.svg` (dark
 * badge, light glyph) for light surfaces, `temps-icon-dark.svg` (light
 * badge, dark glyph) for dark ones. The badge itself never inverts colors
 * relative to its own fill — only which of the two fixed variants is shown.
 *
 * `variant` picks the surface explicitly — use this inside anything that
 * sets its own background independent of the app-wide theme (e.g. a frame
 * that carries its own light/dark toggle). Omit it to fall
 * back to Tailwind's `dark:` variant, which tracks the app-wide theme —
 * correct for use elsewhere in this app (Layout, Brand page).
 */
export function LogoMark({
  className,
  size = 32,
  variant,
}: {
  className?: string
  size?: number
  variant?: 'light' | 'dark'
}) {
  if (variant) {
    return (
      <img
        src={variant === 'dark' ? '/logo/temps-icon-dark.svg' : '/logo/temps-icon.svg'}
        alt="Temps"
        width={size}
        height={size}
        className={className}
        style={{ width: size, height: size }}
      />
    )
  }
  return (
    <>
      <img
        src="/logo/temps-icon.svg"
        alt="Temps"
        width={size}
        height={size}
        className={cn(className, 'dark:hidden')}
        style={{ width: size, height: size }}
      />
      <img
        src="/logo/temps-icon-dark.svg"
        alt="Temps"
        width={size}
        height={size}
        className={cn(className, 'hidden dark:block')}
        style={{ width: size, height: size }}
      />
    </>
  )
}

export function Wordmark({
  className,
  markSize = 28,
  textClassName,
  variant,
}: {
  className?: string
  markSize?: number
  textClassName?: string
  variant?: 'light' | 'dark'
}) {
  return (
    <span className={cn('inline-flex items-center gap-2', className)}>
      <LogoMark size={markSize} variant={variant} />
      <span className={cn('text-lg font-bold tracking-tight', textClassName)}>
        temps
      </span>
    </span>
  )
}
