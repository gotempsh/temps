// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { saveRowClassName, useOnSettingsSurface } from '@temps-sdk/ds'

import { cn } from '@/lib/utils'

/**
 * Whether the nearest enclosing card renders flat. A card placed directly in
 * a settings group (a settings surface) drops its frame, so a settings page
 * is never a box inside a box; cards nested inside that one keep theirs.
 */
const CardFlatContext = React.createContext<boolean | null>(null)

function useCardFlat() {
  return React.useContext(CardFlatContext) === true
}

const Card = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => {
  const onSurface = useOnSettingsSurface()
  const insideCard = React.useContext(CardFlatContext) !== null
  const flat = onSurface && !insideCard
  return (
    <div
      ref={ref}
      className={cn(
        'rounded-lg border bg-card text-card-foreground shadow-sm',
        className,
        flat && 'rounded-none border-0 bg-transparent p-0 shadow-none'
      )}
      {...props}
    >
      <CardFlatContext.Provider value={flat}>
        {children}
      </CardFlatContext.Provider>
    </div>
  )
})
Card.displayName = 'Card'

const CardHeader = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => {
  const flat = useCardFlat()
  return (
    <div
      ref={ref}
      className={cn(
        'flex flex-col space-y-1.5 p-6',
        className,
        flat && 'border-b-0 px-0 pb-4 pt-0'
      )}
      {...props}
    />
  )
})
CardHeader.displayName = 'CardHeader'

const CardTitle = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => {
  const flat = useCardFlat()
  return (
    <div
      ref={ref}
      className={cn(
        'text-2xl font-semibold leading-none tracking-tight',
        className,
        // On a settings page the group already has the heading; a card title
        // there is a sub-heading.
        flat && 'text-sm font-semibold leading-normal tracking-normal'
      )}
      {...props}
    />
  )
})
CardTitle.displayName = 'CardTitle'

const CardDescription = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    className={cn('text-sm text-muted-foreground', className)}
    {...props}
  />
))
CardDescription.displayName = 'CardDescription'

const CardContent = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => {
  const flat = useCardFlat()
  return (
    <div
      ref={ref}
      className={cn('p-6 pt-0', className, flat && 'p-0')}
      {...props}
    />
  )
})
CardContent.displayName = 'CardContent'

const CardFooter = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => {
  const flat = useCardFlat()
  return (
    <div
      ref={ref}
      className={cn(
        'flex items-center p-6 pt-0',
        className,
        // On a settings surface the footer is the group's save row: a rule, a
        // hint on the left and Save on the right, like a page's own save.
        flat && cn(saveRowClassName, 'mt-4 px-0 pb-0')
      )}
      {...props}
    />
  )
})
CardFooter.displayName = 'CardFooter'

export { Card, CardHeader, CardFooter, CardTitle, CardDescription, CardContent }
