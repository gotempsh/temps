// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import { Cpu } from 'lucide-react'
import { OTHER_VENDOR_NAME, type ModelVendor } from './model-vendors'

/**
 * A model vendor's mark. The shipped marks are single-colour dark SVGs loaded
 * through `<img>`, which cannot follow the page's text colour, so they sit on a
 * white chip that stays legible in both themes. A vendor without a mark gets
 * its initial on a neutral chip rather than a broken image.
 */
export function ModelVendorLogo({
  vendor,
  size = 18,
  className,
}: {
  vendor: Pick<ModelVendor, 'name' | 'logo'>
  size?: number
  className?: string
}) {
  const pad = Math.max(2, Math.round(size * 0.18))
  if (vendor.name === OTHER_VENDOR_NAME) {
    return (
      <Cpu
        aria-hidden="true"
        className={cn('shrink-0 text-muted-foreground', className)}
        style={{ width: size, height: size }}
      />
    )
  }
  if (vendor.logo) {
    return (
      <span
        className={cn(
          'inline-flex shrink-0 items-center justify-center rounded-[4px] bg-white ring-1 ring-black/10',
          className
        )}
        style={{ width: size, height: size, padding: pad }}
      >
        <img
          src={`/ai-agents/${vendor.logo}`}
          alt=""
          aria-hidden="true"
          width={size - pad * 2}
          height={size - pad * 2}
          className="block"
          style={{ width: size - pad * 2, height: size - pad * 2 }}
        />
      </span>
    )
  }
  return (
    <span
      aria-hidden="true"
      className={cn(
        'inline-flex shrink-0 items-center justify-center rounded-[4px] bg-muted font-semibold uppercase text-muted-foreground ring-1 ring-border',
        className
      )}
      style={{ width: size, height: size, fontSize: Math.round(size * 0.55) }}
    >
      {vendor.name.charAt(0)}
    </span>
  )
}
