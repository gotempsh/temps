// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import * as PopoverPrimitive from '@radix-ui/react-popover'

import { cn } from '@/lib/utils'
import { SettingsSurface } from '@temps-sdk/ds'

const Popover = PopoverPrimitive.Root

// WAI-ARIA combobox pattern: ArrowDown (or ArrowUp) on a collapsed combobox
// opens its popup. Enter/Space already open it because the trigger is a native
// button. Radix's Popover trigger only wires up click, so every
// `<Button role="combobox">` built on it was unreachable by ArrowDown. Handling
// it here, once, keeps every Popover-based picker consistent; non-combobox
// triggers are untouched.
const COMBOBOX_OPEN_KEYS = new Set(['ArrowDown', 'ArrowUp'])

const PopoverTrigger = React.forwardRef<
  React.ElementRef<typeof PopoverPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof PopoverPrimitive.Trigger>
>(({ onKeyDown, ...props }, ref) => (
  <PopoverPrimitive.Trigger
    ref={ref}
    {...props}
    onKeyDown={(event) => {
      onKeyDown?.(event)
      if (event.defaultPrevented) return
      const trigger = event.currentTarget
      if (
        COMBOBOX_OPEN_KEYS.has(event.key) &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        trigger.getAttribute('role') === 'combobox' &&
        trigger.getAttribute('aria-expanded') !== 'true' &&
        !trigger.hasAttribute('disabled')
      ) {
        event.preventDefault()
        trigger.click()
      }
    }}
  />
))
PopoverTrigger.displayName = PopoverPrimitive.Trigger.displayName

const PopoverContent = React.forwardRef<
  React.ElementRef<typeof PopoverPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof PopoverPrimitive.Content>
>(
  (
    {
      className,
      align = 'center',
      sideOffset = 4,
      collisionPadding = 8,
      ...props
    },
    ref
  ) => (
    <SettingsSurface active={false}>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          ref={ref}
          align={align}
          sideOffset={sideOffset}
          collisionPadding={collisionPadding}
          className={cn(
            'z-[10002] w-72 max-w-[calc(100vw-1rem)] rounded-md border bg-popover p-4 text-popover-foreground shadow-md outline-none data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2',
            className
          )}
          {...props}
        />
      </PopoverPrimitive.Portal>
    </SettingsSurface>
  )
)
PopoverContent.displayName = PopoverPrimitive.Content.displayName

export { Popover, PopoverTrigger, PopoverContent }
