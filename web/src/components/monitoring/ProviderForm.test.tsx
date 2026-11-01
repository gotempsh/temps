// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { zodResolver } from '@hookform/resolvers/zod'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { installBrowserWindow } from '@/test/dom'
import { ProviderForm } from './ProviderForm'
import { providerSchema, type ProviderFormData } from './schemas'

installBrowserWindow()
afterEach(cleanup)

type Form = ReturnType<typeof useForm<ProviderFormData>>

let form: Form | undefined

function Harness({ type }: { type: 'email' | 'webhook' }) {
  const created = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: {
      name: 'Ops',
      provider_type: type,
      config: {
        smtp_host: 'smtp.example.test',
        smtp_port: 587,
        url: 'https://hooks.example.test/in',
        timeout_secs: 30,
        to_addresses: ['ops@example.test'],
        from_address: 'alerts@example.test',
      },
    },
  })
  useEffect(() => {
    form = created
  }, [created])
  return (
    <ProviderForm
      form={created}
      onSubmit={async () => {}}
      hideProviderType
      hideSubmit
    />
  )
}

function box(view: ReturnType<typeof render>, label: RegExp) {
  return view.getByLabelText(label) as HTMLInputElement
}

test('SMTP port stays empty after being changed and then cleared', () => {
  const view = render(<Harness type="email" />)
  const port = box(view, /SMTP port/i)
  expect(port.value).toBe('587')
  fireEvent.change(port, { target: { value: '25' } })
  expect(port.value).toBe('25')
  fireEvent.change(port, { target: { value: '' } })
  expect(port.value).toBe('')
  fireEvent.change(port, { target: { value: '2525' } })
  expect(port.value).toBe('2525')
  expect(form?.getValues('config.smtp_port')).toBe(2525)
})

test('an emptied SMTP port fails validation rather than submitting the old default', async () => {
  const view = render(<Harness type="email" />)
  fireEvent.change(box(view, /SMTP port/i), { target: { value: '25' } })
  fireEvent.change(box(view, /SMTP port/i), { target: { value: '' } })
  expect(box(view, /SMTP port/i).value).toBe('')
  let valid = true
  await act(async () => {
    valid = (await form?.trigger()) ?? true
  })
  expect(valid).toBe(false)
})

test('webhook timeout stays empty after being changed and then cleared', () => {
  const view = render(<Harness type="webhook" />)
  const timeout = box(view, /Timeout/i)
  fireEvent.change(timeout, { target: { value: '60' } })
  fireEvent.change(timeout, { target: { value: '' } })
  expect(timeout.value).toBe('')
})
