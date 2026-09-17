// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { forwardRef } from 'react'
import { Loader2 } from 'lucide-react'
import { Button as BaseButton, type ButtonProps as BaseButtonProps } from '@temps-sdk/ui'
import { cn } from './lib/cn'

export interface ButtonProps extends BaseButtonProps {
  /**
   * True while the action this button triggers is in flight. Renders a
   * spinner and `busyLabel` (falling back to the normal children), and
   * blocks re-submission — but never sets the native `disabled` attribute.
   * A disabled button drops keyboard focus mid-action and some screen
   * readers stop announcing it, right when the user most needs to know
   * their click registered. Use `aria-disabled` + a no-op click guard
   * instead, so the button stays focused, visible, and honestly labeled.
   */
  busy?: boolean
  busyLabel?: React.ReactNode
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  ({ busy = false, busyLabel, children, className, onClick, asChild, ...props }, ref) => {
    const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
      if (busy) {
        event.preventDefault()
        return
      }
      onClick?.(event)
    }

    // `asChild` hands rendering to Radix `Slot`, which requires exactly one
    // React element child to merge its props onto — the spinner/busyLabel
    // composition below would add a sibling node and break that contract
    // (a busy `asChild` button is also a contradiction in terms: `asChild`
    // means "render as this other element", not "render a spinner inside
    // it"), so pass `children` straight through unmodified in that case.
    if (asChild) {
      return (
        <BaseButton
          ref={ref}
          asChild
          aria-busy={busy}
          aria-disabled={busy || props['aria-disabled']}
          className={cn(busy && 'cursor-wait', className)}
          onClick={handleClick}
          {...props}
        >
          {children}
        </BaseButton>
      )
    }

    return (
      <BaseButton
        ref={ref}
        aria-busy={busy}
        aria-disabled={busy || props['aria-disabled']}
        className={cn(busy && 'cursor-wait', className)}
        onClick={handleClick}
        {...props}
      >
        {busy ? <Loader2 className="animate-spin" /> : null}
        {busy ? (busyLabel ?? children) : children}
      </BaseButton>
    )
  },
)
Button.displayName = 'Button'
