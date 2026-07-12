import * as React from 'react'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import { DayPicker, type DayButtonProps } from 'react-day-picker'

import { cn } from '@/lib/utils'
import { buttonVariants } from '@/components/ui/button'

export type CalendarProps = React.ComponentProps<typeof DayPicker>

// react-day-picker v10 stopped merging selection/state modifiers into the
// DayButton's className (only the wrapping "day" cell gets them). Read the
// modifiers directly so the button keeps the same selected/range/today/
// outside/disabled styling it had under the v8 API.
function CalendarDayButton({
  className,
  day: _day,
  modifiers,
  ...props
}: DayButtonProps) {
  const ref = React.useRef<HTMLButtonElement>(null)
  React.useEffect(() => {
    if (modifiers.focused) ref.current?.focus()
  }, [modifiers.focused])

  // A range's start/end anchors render solid (same as a single selected
  // day); only the days between them render in the softer "accent" tone.
  const isSolidSelected = modifiers.selected && !modifiers.range_middle

  return (
    <button
      ref={ref}
      className={cn(
        buttonVariants({ variant: 'ghost' }),
        'h-9 w-9 p-0 font-normal aria-selected:opacity-100',
        isSolidSelected &&
          'bg-primary text-primary-foreground hover:bg-primary hover:text-primary-foreground focus:bg-primary focus:text-primary-foreground',
        modifiers.range_middle && 'bg-accent text-accent-foreground',
        modifiers.today &&
          !modifiers.selected &&
          'bg-accent text-accent-foreground',
        modifiers.outside && 'text-muted-foreground',
        modifiers.disabled && 'text-muted-foreground opacity-50',
        className
      )}
      {...props}
    />
  )
}

function Calendar({
  className,
  classNames,
  showOutsideDays = true,
  ...props
}: CalendarProps) {
  return (
    <DayPicker
      showOutsideDays={showOutsideDays}
      className={cn('p-3', className)}
      classNames={{
        months: 'flex flex-col sm:flex-row space-y-4 sm:space-x-4 sm:space-y-0',
        month: 'space-y-4',
        month_caption: 'flex justify-center pt-1 relative items-center',
        caption_label: 'text-sm font-medium',
        nav: 'space-x-1 flex items-center',
        button_previous: cn(
          buttonVariants({ variant: 'outline' }),
          'absolute left-1 h-7 w-7 bg-transparent p-0 opacity-50 hover:opacity-100'
        ),
        button_next: cn(
          buttonVariants({ variant: 'outline' }),
          'absolute right-1 h-7 w-7 bg-transparent p-0 opacity-50 hover:opacity-100'
        ),
        month_grid: 'w-full border-collapse space-y-1',
        weekdays: 'flex',
        weekday:
          'text-muted-foreground rounded-md w-9 font-normal text-[0.8rem]',
        week: 'flex w-full mt-2',
        day: 'h-9 w-9 text-center text-sm p-0 relative focus-within:relative focus-within:z-20',
        hidden: 'invisible',
        ...classNames,
      }}
      components={{
        Chevron: ({ className, orientation, style }) => {
          const Icon = orientation === 'left' ? ChevronLeft : ChevronRight
          return <Icon className={cn('h-4 w-4', className)} style={style} />
        },
        DayButton: CalendarDayButton,
      }}
      {...props}
    />
  )
}
Calendar.displayName = 'Calendar'

export { Calendar }
