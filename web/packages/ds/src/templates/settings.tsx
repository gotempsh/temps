// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { FormEvent, ReactNode } from 'react'
import { PageContainer, PageHeader } from '../page-header'
import { FormErrors } from '../field'
import { Button } from '../button'
import { cn } from '../lib/cn'

export interface SettingsProps {
  title: ReactNode
  description?: ReactNode
  /** Field-label -> message, e.g. from react-hook-form's `formState.errors`. */
  errors?: Record<string, string | undefined>
  onSubmit: (event: FormEvent<HTMLFormElement>) => void
  /** The form's `Field`s. */
  children: ReactNode
  saving?: boolean
  /** Disables the save bar until something has actually changed. */
  dirty?: boolean
  onCancel?: () => void
  /** The save button's label; name the outcome ("Save repository"). */
  saveLabel?: ReactNode
  /** A section inside an existing page: no page gutters or second h1. */
  embedded?: boolean
  className?: string
}

/**
 * The settings/form template: `Field`s in the body, `FormErrors` above the
 * sticky save bar rather than inline-only, so a long form's errors are
 * visible without scrolling back up. The save bar always stays mounted
 * (never appears/disappears based on `dirty`) so its position doesn't jump
 * around as the user edits.
 */
export function Settings({
  title,
  description,
  errors = {},
  onSubmit,
  children,
  saving = false,
  dirty = true,
  onCancel,
  saveLabel = 'Save changes',
  className,
  embedded = false,
}: SettingsProps) {
  const canSave = dirty && !Object.values(errors).some(Boolean)
  const Container = embedded ? 'section' : PageContainer
  return (
    <Container
      className={cn(
        embedded &&
          'min-w-0 overflow-clip rounded-lg border bg-card text-card-foreground',
        className
      )}
    >
      {embedded ? (
        <div className="space-y-1 p-5">
          <h2 className="text-lg font-semibold">{title}</h2>
          {description && (
            <p className="text-sm text-muted-foreground">{description}</p>
          )}
        </div>
      ) : (
        <PageHeader title={title} description={description} />
      )}
      <form
        onSubmit={(event) => {
          if (saving || !canSave) {
            event.preventDefault()
            return
          }
          onSubmit(event)
        }}
        className={cn('space-y-6', !embedded && 'pb-20')}
      >
        {Object.values(errors).some(Boolean) && (
          <div className={cn(embedded && 'px-5')}>
            <FormErrors errors={errors} />
          </div>
        )}
        <div className={cn('space-y-6', embedded && 'px-5')}>{children}</div>
        <div
          className={cn(
            'sticky bottom-0 flex items-center justify-end gap-2 border-t bg-background/95 py-3 backdrop-blur',
            embedded ? 'px-5' : '-mx-4 px-4 sm:-mx-6 sm:px-6 lg:-mx-8 lg:px-8'
          )}
        >
          {onCancel ? (
            <Button type="button" variant="outline" onClick={onCancel}>
              Cancel
            </Button>
          ) : null}
          <Button
            type="submit"
            busy={saving}
            busyLabel="Saving…"
            aria-disabled={!canSave}
            className={cn(
              !canSave && !saving && 'pointer-events-none opacity-50'
            )}
          >
            {saveLabel}
          </Button>
        </div>
      </form>
    </Container>
  )
}
