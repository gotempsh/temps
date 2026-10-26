// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * LogIngestionBudgetCard — a project's or managed service's own container
 * log ingestion budget, overriding the platform one from Settings →
 * Monitoring. Blank inherits the platform budget; 0 lifts the limit.
 */

import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Button, Field, FormErrors, SettingsGroup } from '@temps-sdk/ds'
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
    // A refetch of the saved budget (after a save, or any other refresh of
    // the parent's query) must not wipe a field the user is still editing.
    resetOptions: { keepDirtyValues: true },
  })
  const { errors } = form.formState

  const handleSave = async (values: FormValues) => {
    try {
      await onSave({
        lines: parseBudget(values.lines, MAX_LINES_PER_SEC) ?? null,
        kib: parseBudget(values.kib, MAX_KIB_PER_SEC) ?? null,
      })
    } catch {
      // The error is surfaced by the caller's toast; swallowing it here keeps
      // react-hook-form from rethrowing it as an unhandled rejection. The
      // fields stay dirty, so the user's values are kept for a retry.
      return
    }
    // Saved: the submitted values are now the baseline, so the fields stop
    // counting as edited and a later refresh of the saved budget shows.
    // `keepDirtyValues: false` because react-hook-form merges `resetOptions`
    // into every reset, which would otherwise keep them dirty.
    form.reset(values, { keepDirtyValues: false })
  }

  const settingsHref =
    platform.data?.setup_path ??
    '/settings/metrics-monitoring#container-log-ingest-budget'
  const platformKib = platform.data
    ? Math.round(platform.data.bytes_per_sec / 1024)
    : undefined

  return (
    <Card className="shadow-none">
      <CardContent className="p-4 md:p-6">
        <form onSubmit={form.handleSubmit(handleSave)} noValidate>
          <SettingsGroup
            title="Container log ingestion"
            description={`Each container may write this many log lines and KiB per second before Temps drops the excess and records a "temps: dropped N log lines" notice. Raise it for a ${scope} that is legitimately chatty, or lower it to keep one noisy ${scope} from filling the log store. Applied to running containers within a minute.`}
          >
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
                  readOnly={isSaving}
                />
                <BudgetField
                  form={form}
                  name="kib"
                  label="KiB per second"
                  max={MAX_KIB_PER_SEC}
                  platform={platformKib}
                  unit="KiB/s"
                  readOnly={isSaving}
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
            <FormErrors
              errors={{
                'Lines per second': errors.lines?.message,
                'KiB per second': errors.kib?.message,
              }}
            />
            <Button
              type="submit"
              disabled={value === undefined}
              busy={isSaving}
              busyLabel="Saving…"
            >
              Save budget
            </Button>
          </SettingsGroup>
        </form>
      </CardContent>
    </Card>
  )
}

function BudgetField({
  form,
  name,
  label,
  max,
  platform,
  unit,
  readOnly,
}: {
  form: ReturnType<typeof useForm<FormValues>>
  name: keyof FormValues
  label: string
  max: number
  platform: number | undefined
  unit: string
  readOnly: boolean
}) {
  const raw = form.watch(name)
  return (
    <Field
      label={label}
      optional
      description={describeBudget(parseBudget(raw, max), platform, unit)}
      error={form.formState.errors[name]?.message}
    >
      {(props) => (
        <Input
          {...props}
          type="number"
          inputMode="numeric"
          min={0}
          max={max}
          step={1}
          readOnly={readOnly}
          placeholder={describeBudget(null, platform, unit)}
          {...form.register(name)}
        />
      )}
    </Field>
  )
}
