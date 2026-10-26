// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * LogIngestionBudgetCard — a project's or managed service's own container
 * log ingestion budget, overriding the platform one from Settings →
 * Monitoring. Blank inherits the platform budget; 0 lifts the limit.
 */

import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { useLogIngestBudget } from '@/hooks/useGlobalLogs'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router'
import { z } from 'zod'
import {
  MAX_KIB_PER_SEC,
  MAX_LINES_PER_SEC,
  budgetInputValue,
  describeBudget,
  parseBudget,
} from './log-ingestion-budget'

const field = (max: number) =>
  z.string().refine((raw) => parseBudget(raw, max) !== undefined, {
    message: `A whole number from 0 to ${max.toLocaleString()}, or blank for the platform default`,
  })

const schema = z.object({
  lines: field(MAX_LINES_PER_SEC),
  kib: field(MAX_KIB_PER_SEC),
})

type FormValues = z.infer<typeof schema>

export interface LogIngestionBudget {
  lines: number | null
  kib: number | null
}

interface LogIngestionBudgetCardProps {
  /** Who the budget is for, in the copy: "project" or "service". */
  scope: 'project' | 'service'
  /** Saved override; `undefined` while it loads. */
  value: { lines?: number | null; kib?: number | null } | undefined
  onSave: (budget: LogIngestionBudget) => Promise<unknown>
  isSaving: boolean
}

export function LogIngestionBudgetCard({
  scope,
  value,
  onSave,
  isSaving,
}: LogIngestionBudgetCardProps) {
  // The platform budget, for the "blank = platform default" hints.
  const platform = useLogIngestBudget()
  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    values: {
      lines: budgetInputValue(value?.lines),
      kib: budgetInputValue(value?.kib),
    },
  })

  const handleSave = (values: FormValues) =>
    onSave({
      lines: parseBudget(values.lines, MAX_LINES_PER_SEC) ?? null,
      kib: parseBudget(values.kib, MAX_KIB_PER_SEC) ?? null,
    })

  const settingsHref =
    platform.data?.setup_path ??
    '/settings/metrics-monitoring#container-log-ingest-budget'

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSave)}>
        <Card className="bg-background text-foreground">
          <CardHeader>
            <CardTitle>Container log ingestion</CardTitle>
            <CardDescription>
              Each container may write this many log lines and KiB per second
              before Temps drops the excess and records a{' '}
              <code className="text-xs">temps: dropped N log lines</code>{' '}
              notice. Raise it for a {scope} that is legitimately chatty, or
              lower it to keep one noisy {scope} from filling the log store.
              Applied to running containers within a minute.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {value === undefined ? (
              <div className="grid gap-4 md:grid-cols-2">
                <Skeleton className="h-16 w-full" />
                <Skeleton className="h-16 w-full" />
              </div>
            ) : (
              <div className="grid gap-4 md:grid-cols-2">
                <BudgetField
                  form={form}
                  name="lines"
                  label="Lines per second"
                  max={MAX_LINES_PER_SEC}
                  platform={platform.data?.lines_per_sec}
                  unit="lines/s"
                />
                <BudgetField
                  form={form}
                  name="kib"
                  label="KiB per second"
                  max={MAX_KIB_PER_SEC}
                  platform={
                    platform.data
                      ? Math.round(platform.data.bytes_per_sec / 1024)
                      : undefined
                  }
                  unit="KiB/s"
                />
              </div>
            )}
            <p className="text-xs text-muted-foreground">
              Leave a field blank to use the platform budget (
              <Link to={settingsHref} className="underline">
                Settings → Metrics Monitoring → Container log ingestion budget
              </Link>
              ); enter 0 for no limit.
            </p>
          </CardContent>
          <CardFooter>
            <Button type="submit" disabled={isSaving || value === undefined}>
              Save budget
            </Button>
          </CardFooter>
        </Card>
      </form>
    </Form>
  )
}

function BudgetField({
  form,
  name,
  label,
  max,
  platform,
  unit,
}: {
  form: ReturnType<typeof useForm<FormValues>>
  name: keyof FormValues
  label: string
  max: number
  platform: number | undefined
  unit: string
}) {
  return (
    <FormField
      control={form.control}
      name={name}
      render={({ field }) => (
        <FormItem>
          <FormLabel>{label}</FormLabel>
          <FormControl>
            <Input
              type="number"
              min={0}
              max={max}
              placeholder={describeBudget(null, platform, unit)}
              {...field}
            />
          </FormControl>
          <FormDescription>
            {describeBudget(parseBudget(field.value, max), platform, unit)}
          </FormDescription>
          <FormMessage />
        </FormItem>
      )}
    />
  )
}
