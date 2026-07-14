'use client'

import { format } from 'date-fns'
import { Calendar as CalendarIcon } from 'lucide-react'
import { DateRange } from 'react-day-picker'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Calendar } from '@/components/ui/calendar'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'

interface DateRangePickerProps {
  date?: DateRange
  onDateChange?: (date: DateRange | undefined) => void
  className?: string
  showTime?: boolean
}

// Copy the hours/minutes/seconds/millis from `source` onto the calendar day in
// `target`, returning a new Date. react-day-picker hands back days normalized to
// local midnight, so without this every calendar click would silently wipe the
// time the user set in the time inputs.
function withTimeFrom(target: Date, source: Date): Date {
  const merged = new Date(target)
  merged.setHours(
    source.getHours(),
    source.getMinutes(),
    source.getSeconds(),
    source.getMilliseconds()
  )
  return merged
}

export function DateRangePicker({
  date,
  onDateChange,
  className,
  showTime = false,
}: DateRangePickerProps) {
  // react-day-picker's range onSelect returns days at local midnight. When
  // showTime is on we preserve whatever time-of-day the user already chose
  // (carried on the previous from/to), and default a fresh end day to the end
  // of that day so the selected last day's data is actually included rather
  // than the range collapsing to the start of it.
  const handleSelect = (range: DateRange | undefined) => {
    if (!onDateChange) return
    if (!showTime || !range) {
      onDateChange(range)
      return
    }
    const from = range.from
      ? withTimeFrom(
          range.from,
          // Keep the previous start time if we had one; otherwise start of day.
          date?.from ?? new Date(new Date(range.from).setHours(0, 0, 0, 0))
        )
      : undefined
    const to = range.to
      ? withTimeFrom(
          range.to,
          // Keep the previous end time if we had one; otherwise end of day so
          // the full selected last day is covered.
          date?.to ?? new Date(new Date(range.to).setHours(23, 59, 59, 999))
        )
      : undefined
    onDateChange({ from, to })
  }

  return (
    <div className={cn('grid gap-2', className)}>
      <Popover>
        <PopoverTrigger asChild>
          <Button
            id="date"
            variant={'outline'}
            className={cn(
              'w-full justify-start text-left font-normal',
              !date && 'text-muted-foreground'
            )}
          >
            <CalendarIcon className="mr-2 h-4 w-4 shrink-0" />
            <span className="truncate">
              {date?.from ? (
                date.to ? (
                  <>
                    {format(
                      date.from,
                      showTime ? 'LLL dd, y HH:mm' : 'LLL dd, y'
                    )}{' '}
                    -{' '}
                    {format(
                      date.to,
                      showTime ? 'LLL dd, y HH:mm' : 'LLL dd, y'
                    )}
                  </>
                ) : (
                  format(date.from, showTime ? 'LLL dd, y HH:mm' : 'LLL dd, y')
                )
              ) : (
                <span>Pick a date range</span>
              )}
            </span>
          </Button>
        </PopoverTrigger>
        <PopoverContent className="w-auto p-0" align="start" sideOffset={4}>
          <Calendar
            autoFocus
            mode="range"
            defaultMonth={date?.from}
            selected={date}
            onSelect={handleSelect}
            numberOfMonths={2}
            className="max-w-screen"
          />
          {showTime && (
            <div className="border-t p-3 flex items-end gap-4">
              <div className="flex-1 space-y-1">
                <Label className="text-xs text-muted-foreground">
                  Start time
                </Label>
                <Input
                  type="time"
                  className="h-8 text-xs"
                  value={
                    date?.from ? format(date.from, 'HH:mm') : '00:00'
                  }
                  onChange={(e) => {
                    if (!date?.from || !onDateChange) return
                    const [hours, minutes] = e.target.value
                      .split(':')
                      .map(Number)
                    const updated = new Date(date.from)
                    updated.setHours(hours, minutes, 0, 0)
                    onDateChange({ from: updated, to: date.to })
                  }}
                  disabled={!date?.from}
                />
              </div>
              <div className="flex-1 space-y-1">
                <Label className="text-xs text-muted-foreground">
                  End time
                </Label>
                <Input
                  type="time"
                  className="h-8 text-xs"
                  value={
                    date?.to ? format(date.to, 'HH:mm') : '23:59'
                  }
                  onChange={(e) => {
                    if (!date?.to || !onDateChange) return
                    const [hours, minutes] = e.target.value
                      .split(':')
                      .map(Number)
                    const updated = new Date(date.to)
                    updated.setHours(hours, minutes, 59, 999)
                    onDateChange({ from: date.from, to: updated })
                  }}
                  disabled={!date?.to}
                />
              </div>
            </div>
          )}
        </PopoverContent>
      </Popover>
    </div>
  )
}
