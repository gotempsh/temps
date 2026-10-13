// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import * as SelectPrimitive from '@radix-ui/react-select'
import { Check, ChevronDown, ChevronUp } from 'lucide-react'

import { cn } from '@/lib/utils'

// Radix renders a hidden native <select> when the Select sits inside a <form>.
// That input echoes programmatic value changes (e.g. react-hook-form reset()
// after settings load) back through a synthetic change event; when the new
// value's <option> isn't committed yet, the native select coerces to "" and
// Radix emits onValueChange(""), clobbering controlled form state and leaving
// the trigger blank. Item values are documented non-empty, so "" can never be
// a real selection — drop it. (Reproduced with @radix-ui/react-select 2.3.5.)
const Select = ({
  onValueChange,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Root>) => (
  <SelectPrimitive.Root
    {...props}
    onValueChange={(value) => {
      if (value === '') return
      onValueChange?.(value)
    }}
  />
)

const SelectGroup = SelectPrimitive.Group

const SelectValue = SelectPrimitive.Value

/**
 * True when something other than the trigger's own content already names it:
 * a `<label for>`, a wrapping `<label>`, or `aria-label(ledby)`.
 */
function hasExternalName(trigger: HTMLElement): boolean {
  if ((trigger as HTMLButtonElement).labels?.length) return true
  if (trigger.closest('label')) return true
  return (
    !!trigger.id &&
    !!trigger.ownerDocument.querySelector(
      `label[for="${trigger.id.replace(/"/g, '\\"')}"]`
    )
  )
}

// A Radix Select trigger is `<button role="combobox">`, and the combobox role
// takes its name from the author, not from its content: with no label, the
// control that visibly reads "All environments" has no accessible name at all
// (axe `button-name`). Call sites should still say what the field is
// (`aria-label` or a `<Label htmlFor>`); this only guarantees that a trigger
// nobody labelled is named by the `<SelectValue>` it shows, through a live
// `aria-labelledby` reference, rather than being announced as a bare
// "combobox". (A reference to the trigger itself is not enough: axe ignores
// self-references.)
const SelectTrigger = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Trigger>
>(
  (
    {
      className,
      children,
      id,
      'aria-label': ariaLabel,
      'aria-labelledby': ariaLabelledBy,
      ...props
    },
    forwardedRef
  ) => {
    const generatedId = React.useId()
    const triggerId = id ?? generatedId
    const valueId = `${triggerId}-value`
    const innerRef = React.useRef<HTMLButtonElement | null>(null)
    const [nameFromValue, setNameFromValue] = React.useState(false)
    const authorNamed = !!ariaLabel?.trim() || !!ariaLabelledBy?.trim()

    // Re-checked whenever the parent re-renders (`children` is a new element
    // each time): a `<label for>` can appear after the trigger.
    React.useEffect(() => {
      const trigger = innerRef.current
      setNameFromValue(!authorNamed && !!trigger && !hasExternalName(trigger))
    }, [authorNamed, children, triggerId])

    const setRefs = React.useCallback(
      (node: HTMLButtonElement | null) => {
        innerRef.current = node
        if (typeof forwardedRef === 'function') forwardedRef(node)
        else if (forwardedRef) forwardedRef.current = node
      },
      [forwardedRef]
    )

    // Give the value a stable id so the trigger can be labelled by it.
    const content = React.Children.map(children, (child) =>
      React.isValidElement<{ id?: string }>(child) &&
      child.type === SelectPrimitive.Value &&
      !child.props.id
        ? React.cloneElement(child, { id: valueId })
        : child
    )

    return (
      <SelectPrimitive.Trigger
        ref={setRefs}
        id={triggerId}
        aria-label={ariaLabel}
        aria-labelledby={nameFromValue ? valueId : ariaLabelledBy}
        className={cn(
          'flex h-10 w-full items-center justify-between rounded-md border border-input bg-background aria-invalid:border-destructive aria-invalid:ring-destructive/20 px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 [&>span]:line-clamp-1',
          className
        )}
        {...props}
      >
        {content}
        <SelectPrimitive.Icon asChild>
          <ChevronDown className="h-4 w-4 opacity-50" />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
    )
  }
)
SelectTrigger.displayName = SelectPrimitive.Trigger.displayName

const SelectScrollUpButton = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.ScrollUpButton>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.ScrollUpButton>
>(({ className, ...props }, ref) => (
  <SelectPrimitive.ScrollUpButton
    ref={ref}
    className={cn(
      'flex cursor-default items-center justify-center py-1',
      className
    )}
    {...props}
  >
    <ChevronUp className="h-4 w-4" />
  </SelectPrimitive.ScrollUpButton>
))
SelectScrollUpButton.displayName = SelectPrimitive.ScrollUpButton.displayName

const SelectScrollDownButton = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.ScrollDownButton>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.ScrollDownButton>
>(({ className, ...props }, ref) => (
  <SelectPrimitive.ScrollDownButton
    ref={ref}
    className={cn(
      'flex cursor-default items-center justify-center py-1',
      className
    )}
    {...props}
  >
    <ChevronDown className="h-4 w-4" />
  </SelectPrimitive.ScrollDownButton>
))
SelectScrollDownButton.displayName =
  SelectPrimitive.ScrollDownButton.displayName

const SelectContent = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Content>
>(({ className, children, position = 'popper', ...props }, ref) => (
  <SelectPrimitive.Portal>
    <SelectPrimitive.Content
      ref={ref}
      className={cn(
        'relative z-[10002] max-h-96 min-w-32 overflow-hidden rounded-md border bg-popover text-popover-foreground shadow-md data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2',
        position === 'popper' &&
          'data-[side=bottom]:translate-y-1 data-[side=left]:-translate-x-1 data-[side=right]:translate-x-1 data-[side=top]:-translate-y-1',
        className
      )}
      position={position}
      {...props}
    >
      <SelectScrollUpButton />
      <SelectPrimitive.Viewport
        className={cn(
          'p-1',
          position === 'popper' &&
            'h-(--radix-select-trigger-height) w-full min-w-(--radix-select-trigger-width)'
        )}
      >
        {children}
      </SelectPrimitive.Viewport>
      <SelectScrollDownButton />
    </SelectPrimitive.Content>
  </SelectPrimitive.Portal>
))
SelectContent.displayName = SelectPrimitive.Content.displayName

const SelectLabel = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Label>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Label>
>(({ className, ...props }, ref) => (
  <SelectPrimitive.Label
    ref={ref}
    className={cn('py-1.5 pl-8 pr-2 text-sm font-semibold', className)}
    {...props}
  />
))
SelectLabel.displayName = SelectPrimitive.Label.displayName

const SelectItem = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Item>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Item>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Item
    ref={ref}
    className={cn(
      'relative flex w-full cursor-default select-none items-center rounded-sm py-1.5 pl-8 pr-2 text-sm outline-none focus:bg-accent focus:text-accent-foreground data-disabled:pointer-events-none data-disabled:opacity-50',
      className
    )}
    {...props}
  >
    <span className="absolute left-2 flex h-3.5 w-3.5 items-center justify-center">
      <SelectPrimitive.ItemIndicator>
        <Check className="h-4 w-4" />
      </SelectPrimitive.ItemIndicator>
    </span>

    <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
  </SelectPrimitive.Item>
))
SelectItem.displayName = SelectPrimitive.Item.displayName

const SelectSeparator = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Separator>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Separator>
>(({ className, ...props }, ref) => (
  <SelectPrimitive.Separator
    ref={ref}
    className={cn('-mx-1 my-1 h-px bg-muted', className)}
    {...props}
  />
))
SelectSeparator.displayName = SelectPrimitive.Separator.displayName

export {
  Select,
  SelectGroup,
  SelectValue,
  SelectTrigger,
  SelectContent,
  SelectLabel,
  SelectItem,
  SelectSeparator,
  SelectScrollUpButton,
  SelectScrollDownButton,
}
