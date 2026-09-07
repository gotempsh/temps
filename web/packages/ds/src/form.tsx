// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Callout } from './callout'

/**
 * One failed field, as the form's summary lists it. `id` is the control's id
 * — the same one handed to `Field` — so the entry can move focus to it.
 */
export type FieldError = { id: string; label: string; message: string }

/**
 * The summary a form shows when more than one field fails on submit: one
 * error Callout at the top of the form, each entry a button that focuses the
 * field it names. A fault is a Callout, never a toast and never a raised
 * panel, and the inline message under each field stays where it is — the
 * summary is a way in, not a second copy of the truth.
 *
 * Renders nothing below `min` failures (one bad field is already marked in
 * place; a summary of one is a second sentence saying the same thing).
 */
export function FormErrors({ errors, title, min = 2, onFocusField, className }: {
  errors: FieldError[]
  /** Overrides the counted sentence. Keep it a verdict, not a heading. */
  title?: string
  /** Failures needed before the summary shows. Default 2. */
  min?: number
  /** Defaults to focusing `document.getElementById(id)`. */
  onFocusField?: (id: string) => void
  className?: string
}) {
  if (errors.length < min) return null
  const focus = (id: string) => {
    if (onFocusField) return onFocusField(id)
    const el = document.getElementById(id)
    el?.focus()
    el?.scrollIntoView({ block: 'center', behavior: 'smooth' })
  }
  return (
    <Callout state="error" className={className}
      title={title ?? `${errors.length} field${errors.length > 1 ? 's' : ''} to fix before this saves`}>
      {errors.map((e) => (
        <span key={e.id} className="block">
          <button type="button" onClick={() => focus(e.id)} className="text-foreground underline underline-offset-4 hover:text-foreground">{e.label}</button>
          <span> · {e.message}</span>
        </span>
      ))}
    </Callout>
  )
}
