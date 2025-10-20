'use client'

import * as React from 'react'
import { format } from 'date-fns'
import { Calendar as CalendarIcon } from 'lucide-react'
import { DateRange } from 'react-day-picker'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Calendar } from '@/components/ui/calendar'
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

export function DateRangePicker({
  date,
  onDateChange,
  className,
  showTime = false,
}: DateRangePickerProps) {
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
            initialFocus
            mode="range"
            defaultMonth={date?.from}
            selected={date}
            onSelect={onDateChange}
            numberOfMonths={2}
            className="max-w-screen"
          />
        </PopoverContent>
      </Popover>
    </div>
  )
}
