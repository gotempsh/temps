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
  ({ busy = false, busyLabel, children, className, onClick, ...props }, ref) => {
    return (
      <BaseButton
        ref={ref}
        aria-busy={busy}
        aria-disabled={busy || props['aria-disabled']}
        className={cn(busy && 'cursor-wait', className)}
        onClick={(event) => {
          if (busy) {
            event.preventDefault()
            return
          }
          onClick?.(event)
        }}
        {...props}
      >
        {busy ? <Loader2 className="animate-spin" /> : null}
        {busy ? (busyLabel ?? children) : children}
      </BaseButton>
    )
  },
)
Button.displayName = 'Button'
