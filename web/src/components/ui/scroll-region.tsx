// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'

import { cn } from '@/lib/utils'
import { scrollableRegionProps, useScrollableRegion } from './scrollable-region'

export interface ScrollRegionProps extends React.HTMLAttributes<HTMLDivElement> {
  /**
   * Accessible name for the region while it scrolls, for example the name of
   * the table inside. Optional: without it the container is still keyboard
   * focusable, just unnamed.
   */
  regionLabel?: string
}

/**
 * A scrolling container that keyboard users can scroll (WCAG 2.1.1). Use it
 * instead of a bare `div className="overflow-x-auto"` around content that has
 * nothing focusable inside, such as a plain `<table>` or a block of text. It
 * only becomes a tab stop while the content actually overflows.
 */
export const ScrollRegion = React.forwardRef<HTMLDivElement, ScrollRegionProps>(
  ({ className, regionLabel, children, ...props }, forwardedRef) => {
    const region = useScrollableRegion<HTMLDivElement>(forwardedRef)
    return (
      <div
        ref={region.ref}
        className={cn(
          'overflow-auto focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
          className
        )}
        {...props}
        {...scrollableRegionProps(region.scrollable, regionLabel)}
      >
        {children}
      </div>
    )
  }
)
ScrollRegion.displayName = 'ScrollRegion'
