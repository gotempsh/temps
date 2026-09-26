// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Button, Callout, EchoDialog, Field, FormErrors, PageContainer, PageHeader, Picker, Status } from '@temps-sdk/ds'
import { Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@temps-sdk/ui'

const TARGETS = [
  { value: 'api', label: 'Checkout API', description: 'Handles checkout requests', keywords: ['payments'] },
  { value: 'worker', label: 'Background worker', description: 'Processes queued jobs' },
  { value: 'archive', label: 'Archived service', description: 'Restore this service before selecting it', disabled: true },
]

/** Invented data only; no API requests or real mutations. */
export default function Controls() {
  const [name, setName] = useState('checkout-api')
  const [environment, setEnvironment] = useState('preview')
  const [target, setTarget] = useState<string>()
  const [submitted, setSubmitted] = useState(false)
  const [reviewed, setReviewed] = useState(false)
  const [dialog, setDialog] = useState(false)
  const [deleted, setDeleted] = useState(false)
  const errors = {
    'Project name': name.trim() ? undefined : 'Enter a project name.',
    Service: target ? undefined : 'Choose a service from the list.',
  }
  const edit = () => { setReviewed(false); setDeleted(false) }
  return (
    <PageContainer>
      <PageHeader title="Forms and selection" description="Try validation, search, keyboard selection, and confirmation. All changes stay in this example." />
      <div className="grid items-start gap-8 xl:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
        <form className="min-w-0 space-y-6" noValidate onSubmit={event => {
          event.preventDefault()
          setSubmitted(true)
          setReviewed(!Object.values(errors).some(Boolean))
        }}>
          <section aria-labelledby="config-heading" className="space-y-5 rounded-lg border p-5">
            <h2 id="config-heading" className="text-base font-semibold">Project configuration</h2>
            <Field label="Project name" description="Use a name your team will recognize." error={submitted ? errors['Project name'] : undefined}>
              {props => <Input {...props} value={name} onChange={event => { setName(event.target.value); edit() }} />}
            </Field>
            <Field label="Environment" description="Preview keeps this example separate from production.">
              {props => <Select value={environment} onValueChange={value => { setEnvironment(value); edit() }}>
                <SelectTrigger {...props}><SelectValue /></SelectTrigger>
                <SelectContent><SelectItem value="preview">Preview</SelectItem><SelectItem value="production">Production</SelectItem></SelectContent>
              </Select>}
            </Field>
            <Field label="Service" description="Search by name or keyword, then use the arrow keys and Enter." error={submitted ? errors.Service : undefined}>
              {props => <Picker inputProps={props} items={TARGETS} value={target} onValueChange={value => { setTarget(value); edit() }} placeholder="Search services…" emptyMessage="No services match. Try another name or clear your search." />}
            </Field>
            <p role="status" className="text-sm text-muted-foreground">{target ? `Selected: ${TARGETS.find(item => item.value === target)?.label}` : 'No service selected.'}</p>
          </section>
          <FormErrors errors={submitted ? errors : {}} />
          <div className="flex flex-wrap items-center gap-3">
            <Button type="submit">Review sample configuration</Button>
            {reviewed ? <Status tone="ok" label="Ready to review" /> : null}
          </div>
          {reviewed ? <Callout title="Sample configuration is valid">{name} · {environment}. No project was created or updated.</Callout> : null}
        </form>
        <section aria-labelledby="confirmation-heading" className="min-w-0 space-y-4 rounded-lg border p-5">
          <h2 id="confirmation-heading" className="text-base font-semibold">Destructive confirmation</h2>
          <p className="text-sm text-muted-foreground">Copy the name, then paste or type it to confirm. Try a long project name to check wrapping.</p>
          <Button variant="outline" onClick={() => setDialog(true)} disabled={!name.trim()}>Preview delete dialog</Button>
          <p role="status" className="text-sm text-muted-foreground">{deleted ? 'Sample confirmation completed. Nothing was deleted.' : 'This dialog has no connection to your projects.'}</p>
          <EchoDialog open={dialog} onOpenChange={setDialog} title={`Delete ${name}?`} description="This example demonstrates the confirmation step. Nothing will be deleted." phrase={name} confirmLabel="Confirm sample deletion" onConfirm={() => { setDialog(false); setDeleted(true) }} />
        </section>
      </div>
    </PageContainer>
  )
}
