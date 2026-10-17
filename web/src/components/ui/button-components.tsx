// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { Slot } from '@radix-ui/react-slot'
import { cn } from '@/lib/utils'
import { buttonVariants, type ButtonProps } from './button-shared'
import { warnUnnamedIconButton } from './accessible-name'

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, ...props }, ref) => {
    const Comp = asChild ? Slot : 'button'
    // An icon-only button has no text for assistive technology to read; flag
    // it while developing (axe `button-name`). `asChild` hands the content to
    // the child element, so only plain buttons are checked.
    if (size === 'icon' && !asChild) warnUnnamedIconButton(props, 'Button')
    return (
      <Comp
        className={cn(buttonVariants({ variant, size, className }))}
        ref={ref}
        {...props}
      />
    )
  }
)

Button.displayName = 'Button'
