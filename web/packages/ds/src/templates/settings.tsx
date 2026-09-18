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
  className,
}: SettingsProps) {
  return (
    <PageContainer className={className}>
      <PageHeader title={title} description={description} />
      <form
        onSubmit={(event) => {
          if (saving || !dirty || Object.values(errors).some(Boolean)) {
            event.preventDefault()
            return
          }
          onSubmit(event)
        }}
        className="space-y-6 pb-20"
      >
        <FormErrors errors={errors} />
        <div className="space-y-6">{children}</div>
        <div className="sticky bottom-0 -mx-4 flex items-center justify-end gap-2 border-t bg-background/95 px-4 py-3 backdrop-blur sm:-mx-6 sm:px-6 lg:-mx-8 lg:px-8">
          {onCancel ? (
            <Button type="button" variant="outline" onClick={onCancel}>
              Cancel
            </Button>
          ) : null}
          <Button
            type="submit"
            busy={saving}
            busyLabel="Saving…"
            aria-disabled={!dirty || Object.values(errors).some(Boolean)}
            className={cn(!dirty && !saving && 'pointer-events-none opacity-50')}
          >
            Save changes
          </Button>
        </div>
      </form>
    </PageContainer>
  )
}
