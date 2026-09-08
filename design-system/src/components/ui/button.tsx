// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { Slot } from '@radix-ui/react-slot'
import { cva, type VariantProps } from 'class-variance-authority'

import { cn } from '@/lib/utils'

const buttonVariants = cva(
  'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium ring-offset-background transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        default: 'bg-primary text-primary-foreground hover:bg-primary/90',
        destructive:
          'bg-destructive text-destructive-foreground hover:bg-destructive/90',
        outline:
          'border border-input bg-background hover:bg-accent hover:text-accent-foreground',
        secondary:
          'bg-secondary text-secondary-foreground hover:bg-secondary/80',
        ghost: 'hover:bg-accent hover:text-accent-foreground',
        link: 'text-primary underline-offset-4 hover:underline',
      },
      size: {
        default: 'h-10 px-4 py-2',
        sm: 'h-9 rounded-md px-3',
        lg: 'h-11 rounded-md px-8',
        icon: 'h-10 w-10',
      },
    },
    defaultVariants: {
      variant: 'default',
      size: 'default',
    },
  }
)

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  asChild?: boolean
  /**
   * The work this button started is still running. This is the one sanctioned
   * spinner in the system: a busy button spins its icon, and nothing else
   * spins. (A `running` glyph pulses instead — a state is not an action.)
   *
   * Busy is deliberately not `disabled`: disabling greys the control out, and
   * — worse — a disabled button drops focus, so a keyboard reader is dumped
   * back to the top of the document at exactly the moment they are waiting to
   * hear what happened. Busy keeps the focus and the colour, sets `aria-busy`,
   * and ignores clicks.
   */
  busy?: boolean
  /** What the label says while busy: "saving…", "deploying…", "reloading…". */
  busyLabel?: React.ReactNode
}

/**
 * The shortest time a button stays busy once it has started, in ms. Under this
 * a fast answer reads as a flicker — the eye registers that something moved
 * without registering what — and the reader cannot tell a success from a
 * no-op. Long enough to be seen, short enough not to be a delay.
 */
const MIN_BUSY_MS = 400

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, busy = false, busyLabel, children, onClick, ...props }, ref) => {
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
          className={cn(buttonVariants({ variant, size, className }), showBusy && !asChild && 'op-busy cursor-default')}
          ref={ref}
          aria-busy={showBusy && !asChild ? true : undefined}
          onClick={showBusy && !asChild ? (e: React.MouseEvent<HTMLButtonElement>) => e.preventDefault() : onClick}
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
        className={cn(buttonVariants({ variant, size, className }), showBusy && 'op-busy cursor-default')}
        ref={ref}
        aria-busy={showBusy || undefined}
        // Not `disabled`: see the `busy` doc above. Clicks are swallowed instead.
        onClick={showBusy ? (e: React.MouseEvent<HTMLButtonElement>) => e.preventDefault() : onClick}
        {...props}
      >
        <span className="grid">
          <span aria-hidden={showBusy || undefined} className={cn('col-start-1 row-start-1 inline-flex items-center justify-center gap-2', showBusy && 'invisible')}>{children}</span>
          <span aria-hidden={!showBusy || undefined} className={cn('col-start-1 row-start-1 inline-flex items-center justify-center gap-2', !showBusy && 'invisible')}>{busyLabel}</span>
        </span>
      </Comp>
    )
  }
)
Button.displayName = 'Button'

export { Button, buttonVariants }
