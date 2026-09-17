// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Field, Settings } from '@temps-sdk/ds'
import { Input } from '@temps-sdk/ui'

const INITIAL = { name: 'checkout-api', notifyEmail: '' }

/** Reference screen for the `Settings` template: `Field`s + sticky save bar. */
export default function ProjectSettings() {
  const [values, setValues] = useState(INITIAL)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)

  const dirty = values.name !== INITIAL.name || values.notifyEmail !== INITIAL.notifyEmail

  const errors: Record<string, string | undefined> = {
    name: values.name.trim().length === 0 ? 'Project name is required.' : undefined,
    notifyEmail:
      values.notifyEmail && !values.notifyEmail.includes('@')
        ? 'Enter a valid email address.'
        : undefined,
  }

  const hasErrors = Object.values(errors).some(Boolean)

  return (
    <Settings
      title="Project settings"
      description="General configuration for this project."
      errors={errors}
      dirty={dirty && !hasErrors}
      saving={saving}
      onSubmit={(e) => {
        e.preventDefault()
        if (hasErrors) return
        setSaving(true)
        setTimeout(() => {
          setSaving(false)
          setSaved(true)
        }, 900)
      }}
    >
      <Field label="Project name" error={errors.name}>
        {(fieldProps) => (
          <Input
            {...fieldProps}
            value={values.name}
            onChange={(e) => setValues((v) => ({ ...v, name: e.target.value }))}
          />
        )}
      </Field>
      <Field
        label="Deploy notification email"
        optional
        description="Sent when a deploy to this project fails."
        error={errors.notifyEmail}
      >
        {(fieldProps) => (
          <Input
            {...fieldProps}
            type="email"
            placeholder="you@example.com"
            value={values.notifyEmail}
            onChange={(e) => setValues((v) => ({ ...v, notifyEmail: e.target.value }))}
          />
        )}
      </Field>
      {saved ? <p className="text-sm text-success">Saved.</p> : null}
    </Settings>
  )
}
