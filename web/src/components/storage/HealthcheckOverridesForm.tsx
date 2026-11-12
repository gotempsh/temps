// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo } from 'react'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import { Field, SaveRow } from '@temps-sdk/ds'
import type {
  HealthcheckOverrides,
  HealthcheckTimings,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  APPLY_ON_RECREATE,
  formValuesOf,
  HEALTHCHECK_FIELDS,
  healthcheckFormSchema,
  overridesOf,
  type HealthcheckFormValues,
} from '@/lib/service-healthcheck'

export interface HealthcheckOverridesFormProps {
  /** The saved overrides (null = inherit). */
  overrides: HealthcheckOverrides
  /**
   * What an empty field falls back to, one entry per engine the values apply
   * to. The first entry's values are shown as placeholders.
   */
  inherited: HealthcheckTimings[]
  /** Shown as each placeholder's source, e.g. "the instance default". */
  inheritedFrom: string
  saving: boolean
  disabled?: boolean
  submitLabel?: string
  onSave: (overrides: HealthcheckOverrides) => void
  /** Told whether the form has unsaved edits. */
  onDirtyChange?: (dirty: boolean) => void
}

/** Five optional healthcheck values; an empty field inherits. */
export function HealthcheckOverridesForm({
  overrides,
  inherited,
  inheritedFrom,
  saving,
  disabled = false,
  submitLabel = 'Save',
  onSave,
  onDirtyChange,
}: HealthcheckOverridesFormProps) {
  const schema = useMemo(() => healthcheckFormSchema(inherited), [inherited])
  const saved = useMemo(() => formValuesOf(overrides), [overrides])
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors, isDirty },
  } = useForm<HealthcheckFormValues>({
    resolver: zodResolver(schema),
    defaultValues: saved,
  })

  useEffect(() => {
    reset(saved)
  }, [saved, reset])

  useEffect(() => {
    onDirtyChange?.(isDirty)
  }, [isDirty, onDirtyChange])

  const placeholderOf = inherited[0]
  const hasOverride = Object.values(saved).some((v) => v !== '')

  return (
    <form
      className="space-y-4"
      aria-label="Healthcheck overrides"
      onSubmit={handleSubmit((values) => onSave(overridesOf(values)))}
    >
      <div className="grid grid-cols-2 gap-4 md:grid-cols-5">
        {HEALTHCHECK_FIELDS.map((spec) => (
          <Field
            key={spec.field}
            label={spec.unit === 's' ? `${spec.label} (s)` : spec.label}
            error={errors[spec.field]?.message}
          >
            {(props) => (
              <Input
                {...props}
                inputMode="numeric"
                autoComplete="off"
                disabled={disabled || saving}
                placeholder={
                  placeholderOf ? String(placeholderOf[spec.field]) : undefined
                }
                title={`Empty: ${inheritedFrom}. ${spec.min}–${spec.max}${spec.unit}`}
                {...register(spec.field)}
              />
            )}
          </Field>
        ))}
      </div>
      <p className="text-xs text-muted-foreground">
        Leave a field empty to use {inheritedFrom}.
      </p>
      {isDirty ? (
        <Button
          type="button"
          size="sm"
          variant="ghost"
          disabled={saving}
          onClick={() => reset(saved)}
        >
          Discard changes
        </Button>
      ) : hasOverride ? (
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={disabled || saving}
          onClick={() => onSave(overridesOf(formValuesOf(undefined)))}
        >
          Clear overrides
        </Button>
      ) : null}
      <SaveRow
        dirty={isDirty}
        saving={saving}
        disabled={disabled}
        label={submitLabel}
        hint={isDirty ? APPLY_ON_RECREATE : undefined}
      />
    </form>
  )
}
