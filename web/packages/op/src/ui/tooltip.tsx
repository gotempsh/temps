// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import * as TooltipPrimitive from '@radix-ui/react-tooltip'

import { cn } from '../lib/cn'

/**
 * A tooltip is a label, not a surface: it appears, it does not fly in, and
 * the pointer never needs to reach it. So no motion, no radius, no shadow,
 * and hoverable content is off for every tooltip, otherwise a pointer that
 * drifts down through the label leaves it open.
 */
const TooltipProvider = (props: React.ComponentProps<typeof TooltipPrimitive.Provider>) => <TooltipPrimitive.Provider disableHoverableContent {...props} />

const Tooltip = TooltipPrimitive.Root

const TooltipTrigger = TooltipPrimitive.Trigger

const TooltipContent = React.forwardRef<
  React.ElementRef<typeof TooltipPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>
>(({ className, sideOffset = 4, ...props }, ref) => (
  <TooltipPrimitive.Content
    ref={ref}
    sideOffset={sideOffset}
    className={cn(
      'z-[9999] overflow-hidden rounded-none border bg-background px-2 py-1 text-xs text-foreground',
      className
    )}
    {...props}
  />
))
TooltipContent.displayName = TooltipPrimitive.Content.displayName

export { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider }
