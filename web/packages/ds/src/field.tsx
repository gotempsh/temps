// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, type ReactNode } from 'react'
import { Label } from '@temps-sdk/ui'
import { HelpPopover } from './help'
import { Callout } from './callout'
import { cn } from './lib/cn'

export interface FieldProps {
  label: ReactNode
  /** Renders the field optional to make required the default, the honest case for most forms. */
  optional?: boolean
  description?: ReactNode
  /** Optional background context; required instructions stay in description. */
  help?: { label: string; content: ReactNode }
  /** A single field-level validation message (e.g. `formState.errors.name?.message`). */
  error?: string
  children: (props: {
    id: string
    'aria-labelledby'?: string
    'aria-describedby'?: string
    'aria-invalid'?: boolean
  }) => ReactNode
  className?: string
}

/**
 * One labeled form control: label, the control (via render prop so `Field`
 * stays agnostic to react-hook-form/plain state), help text, and an error
 * message — wired together with matching `id`/`aria-describedby` so a
 * screen reader announces the error when the control receives focus.
 */
export function Field({
  label,
  optional,
  description,
  help,
  error,
  children,
  className,
}: FieldProps) {
  const id = useId()
  const descId = description ? `${id}-description` : undefined
  const errorId = error ? `${id}-error` : undefined
  const describedBy = [descId, errorId].filter(Boolean).join(' ') || undefined

  return (
    <div className={cn('space-y-1.5', className)}>
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <div className="flex items-center gap-1">
          <Label id={`${id}-label`} htmlFor={id}>
            {label}
          </Label>
          {help ? (
            <HelpPopover label={help.label}>{help.content}</HelpPopover>
          ) : null}
        </div>
        {optional ? (
          <span className="text-xs text-muted-foreground">Optional</span>
        ) : null}
      </div>
      {children({
        id,
        'aria-labelledby': `${id}-label`,
        'aria-describedby': describedBy,
        'aria-invalid': !!error,
      })}
      {description ? (
        <p id={descId} className="text-xs text-muted-foreground">
          {description}
        </p>
      ) : null}
      {error ? (
        <p id={errorId} className="text-xs font-medium text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  )
}

export interface FormErrorsProps {
  /** Field-label -> message. Pass react-hook-form's `formState.errors` mapped to this shape. */
  errors: Record<string, string | undefined>
  className?: string
}

/** A summary of every current validation error, for above a long form's sticky save bar. */
export function FormErrors({ errors, className }: FormErrorsProps) {
  const entries = Object.entries(errors).filter(
    (entry): entry is [string, string] => !!entry[1]
  )
  if (entries.length === 0) return null
  return (
    <Callout
      tone="error"
      title={`Fix ${entries.length} field${entries.length === 1 ? '' : 's'} before saving`}
      className={className}
    >
      <ul className="list-inside list-disc space-y-0.5">
        {entries.map(([field, message]) => (
          <li key={field}>
            <span className="font-medium">{field}:</span> {message}
          </li>
        ))}
      </ul>
    </Callout>
  )
}
