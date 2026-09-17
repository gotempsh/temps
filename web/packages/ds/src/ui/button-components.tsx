// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { Slot } from '@radix-ui/react-slot'
import { cn } from '../lib/cn'
import { buttonVariants, type ButtonProps } from './button-shared'

/**
 * The shortest time a button stays busy once it has started, in ms. Under this
 * a fast answer reads as a flicker — the eye registers that something moved
 * without registering what — and the reader cannot tell a success from a
 * no-op. Long enough to be seen, short enough not to be a delay.
 */
const MIN_BUSY_MS = 400

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  (
    {
      className,
      variant,
      size,
      asChild = false,
      busy = false,
      busyLabel,
      children,
      onClick,
      ...props
    },
    ref
  ) => {
    const Comp = asChild ? Slot : 'button'

    // Hold busy for MIN_BUSY_MS after it is set, so a 40ms answer still reads.
    const [held, setHeld] = React.useState(false)
    React.useEffect(() => {
      if (!busy) return
      setHeld(true)
      const id = setTimeout(() => setHeld(false), MIN_BUSY_MS)
      return () => clearTimeout(id)
    }, [busy])
    const showBusy = busy || held

    if (asChild || busyLabel === undefined) {
      return (
        <Comp
          className={cn(
            buttonVariants({ variant, size, className }),
            showBusy && !asChild && 'op-busy cursor-default'
          )}
          ref={ref}
          aria-busy={showBusy && !asChild ? true : undefined}
          onClick={
            showBusy && !asChild
              ? (e: React.MouseEvent<HTMLButtonElement>) => e.preventDefault()
              : onClick
          }
          {...props}
        >
          {children}
        </Comp>
      )
    }

    /* Both labels live in the same grid cell, so the button is always as wide
       as the wider of the two and "save" becoming "saving…" moves nothing.
       Locking a min-width to the idle width cannot do this: the busy label is
       the longer one, so the button would still grow the first time it runs. */
    return (
      <Comp
        className={cn(
          buttonVariants({ variant, size, className }),
          showBusy && 'op-busy cursor-default'
        )}
        ref={ref}
        aria-busy={showBusy || undefined}
        // Not `disabled`: see the `busy` doc above. Clicks are swallowed instead.
        onClick={
          showBusy
            ? (e: React.MouseEvent<HTMLButtonElement>) => e.preventDefault()
            : onClick
        }
        {...props}
      >
        <span className="grid">
          <span
            aria-hidden={showBusy || undefined}
            className={cn(
              'col-start-1 row-start-1 inline-flex items-center justify-center gap-2',
              showBusy && 'invisible'
            )}
          >
            {children}
          </span>
          <span
            aria-hidden={!showBusy || undefined}
            className={cn(
              'col-start-1 row-start-1 inline-flex items-center justify-center gap-2',
              !showBusy && 'invisible'
            )}
          >
            {busyLabel}
          </span>
        </span>
      </Comp>
    )
  }
)

Button.displayName = 'Button'
