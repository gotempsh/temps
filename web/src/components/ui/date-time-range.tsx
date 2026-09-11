// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { CalendarDays } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  QUICK_TIME_RANGES,
  customTimeRange,
  localDateTime,
  quickTimeRange,
  type DateTimeRangeValue,
  type QuickTimeRange,
} from '@/lib/date-time-range'

/** Compact, controlled range input. Only Apply commits custom drafts. */
export function DateTimeRange({
  value,
  onChange,
  maxRangeDays = 30,
  active = true,
  allowCustom = true,
}: {
  value: DateTimeRangeValue
  onChange: (value: DateTimeRangeValue) => void
  maxRangeDays?: number
  active?: boolean
  allowCustom?: boolean
}) {
  const [open, setOpen] = useState(false)
  const id = useId()
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone
  const schema = useMemo(
    () =>
      z
        .object({ from: z.string(), to: z.string() })
        .superRefine((draft, context) => {
          const result = customTimeRange(draft.from, draft.to, maxRangeDays)
          if ('field' in result)
            context.addIssue({
              code: 'custom',
              path: [result.field],
              message: result.message,
            })
        }),
    [maxRangeDays]
  )
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors },
  } = useForm({
    resolver: zodResolver(schema),
    defaultValues: {
      from: localDateTime(value.from),
      to: localDateTime(value.to),
    },
  })
  const summary = !active
    ? 'All time'
    : `${new Date(value.from).toLocaleString()} – ${new Date(value.to).toLocaleString()} · ${timezone}`
  const changeOpen = (next: boolean) => {
    if (next)
      reset({ from: localDateTime(value.from), to: localDateTime(value.to) })
    setOpen(next)
  }
  return (
    <div
      role="group"
      aria-label="Date and time range"
      aria-describedby={`${id}-current`}
      className="inline-flex h-9 shrink-0 items-center gap-0.5 rounded-md border bg-background p-0.5"
      title={summary}
    >
      <span id={`${id}-current`} className="sr-only">
        Selected range: {summary}
      </span>
      {(Object.keys(QUICK_TIME_RANGES) as QuickTimeRange[]).map((preset) => (
        <Button
          key={preset}
          type="button"
          variant={active && value.preset === preset ? 'secondary' : 'ghost'}
          size="sm"
          className="h-7 px-2.5 text-xs"
          aria-pressed={active && value.preset === preset}
          onClick={() => {
            setOpen(false)
            onChange(quickTimeRange(preset))
          }}
        >
          {preset}
        </Button>
      ))}
      <Popover open={open} onOpenChange={changeOpen}>
        <PopoverTrigger asChild>
          <Button
            type="button"
            variant={
              active && value.preset === 'custom' ? 'secondary' : 'ghost'
            }
            size="sm"
            className="h-7 gap-1.5 px-2.5 text-xs"
            disabled={!allowCustom}
            title={
              !allowCustom
                ? 'This data source supports preset ranges only'
                : undefined
            }
            aria-label="Custom time range"
            aria-pressed={active && value.preset === 'custom'}
          >
            <CalendarDays className="size-3.5" />
            Custom
          </Button>
        </PopoverTrigger>
        <PopoverContent
          align="end"
          collisionPadding={16}
          className="w-80 max-w-[calc(100vw-2rem)]"
          aria-labelledby={`${id}-title`}
        >
          <form
            noValidate
            onSubmit={handleSubmit((draft) => {
              const result = customTimeRange(draft.from, draft.to, maxRangeDays)
              if ('value' in result) {
                onChange(result.value)
                setOpen(false)
              }
            })}
            className="space-y-4"
          >
            <div>
              <h3 id={`${id}-title`} className="text-sm font-semibold">
                Custom time range
              </h3>
              <p className="mt-1 text-xs text-muted-foreground">
                {timezone} · Up to {maxRangeDays} days
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor={`${id}-from`}>Start date and time</Label>
              <Input
                id={`${id}-from`}
                type="datetime-local"
                step="60"
                className="min-w-0 text-sm"
                {...register('from')}
                aria-invalid={Boolean(errors.from)}
                aria-describedby={errors.from ? `${id}-from-error` : undefined}
              />
              {errors.from && (
                <p
                  id={`${id}-from-error`}
                  role="alert"
                  className="text-xs text-destructive"
                >
                  {errors.from.message}
                </p>
              )}
            </div>
            <div className="space-y-1.5">
              <Label htmlFor={`${id}-to`}>End date and time</Label>
              <Input
                id={`${id}-to`}
                type="datetime-local"
                step="60"
                className="min-w-0 text-sm"
                {...register('to')}
                aria-invalid={Boolean(errors.to)}
                aria-describedby={errors.to ? `${id}-to-error` : undefined}
              />
              {errors.to && (
                <p
                  id={`${id}-to-error`}
                  role="alert"
                  className="text-xs text-destructive"
                >
                  {errors.to.message}
                </p>
              )}
            </div>
            <div className="flex justify-end gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => setOpen(false)}
              >
                Cancel
              </Button>
              <Button type="submit" size="sm">
                Apply range
              </Button>
            </div>
          </form>
        </PopoverContent>
      </Popover>
    </div>
  )
}
