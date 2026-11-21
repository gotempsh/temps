// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'

import type { AttachmentUsage } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  AttachmentsSettingsBody,
  ErrorAttachmentsSettings,
} from './ErrorAttachmentsSettings'
import { AttachmentItems } from './EventAttachments'

installBrowserWindow()
afterEach(cleanup)

const MIB = 1_048_576
const USAGE_PATH = '/projects/3/error-attachments/usage'
const QUOTA_PATH = '/projects/3/error-attachments/quota'

const usage = (overrides: Partial<AttachmentUsage> = {}): AttachmentUsage => ({
  configured: true,
  reason: null,
  setup_path: null,
  used_bytes: 0,
  files: 0,
  quota_bytes: 256 * MIB,
  max_attachment_bytes: 2 * MIB,
  max_event_attachment_bytes: 4 * MIB,
  max_attachments_per_event: 8,
  dropped_over_quota: 0,
  dropped_queue_full: 0,
  ...overrides,
})

function wrap(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>{children}</MemoryRouter>
    </QueryClientProvider>
  )
}

const saveButton = (view: ReturnType<typeof render>) =>
  view.getByRole('button', { name: /^Save/ }) as HTMLButtonElement

test('a quota that is not whole MiB is kept until the user enters a new one', async () => {
  mockApi({
    [`GET ${USAGE_PATH}`]: () => jsonResponse(usage({ quota_bytes: 1 })),
  })
  const view = render(wrap(<ErrorAttachmentsSettings projectId={3} />))

  await view.findByText(/The current quota, 1 B, is not a whole number/)
  expect(saveButton(view).disabled).toBe(true)
  fireEvent.change(view.getByLabelText('Quota (MiB)'), {
    target: { value: '5' },
  })
  await waitFor(() => expect(saveButton(view).disabled).toBe(false))
})

test('a failed save shows beside Save and waits for the saved quota to be read again', async () => {
  let reads = 0
  let rereadFails = true
  const api = mockApi({
    [`GET ${USAGE_PATH}`]: () => {
      reads += 1
      if (reads > 1 && rereadFails) {
        return jsonResponse({ title: 'Unavailable', status: 503 }, 503)
      }
      return jsonResponse(usage())
    },
    [`PUT ${QUOTA_PATH}`]: () =>
      jsonResponse(
        { title: 'Unavailable', status: 503, detail: 'Database is restarting' },
        503
      ),
  })
  const view = render(wrap(<ErrorAttachmentsSettings projectId={3} />))
  const input = (await view.findByLabelText('Quota (MiB)')) as HTMLInputElement
  fireEvent.change(input, { target: { value: '512' } })
  await waitFor(() => expect(saveButton(view).disabled).toBe(false))
  fireEvent.click(saveButton(view))

  // The failure is next to Save, the edit is kept, and the re-read failed,
  // so another save waits for a read that works.
  await view.findByText('The attachment quota may not have been saved')
  expect(view.getByText(/Database is restarting/)).toBeTruthy()
  await view.findByText(/could not be refreshed/)
  expect(input.value).toBe('512')
  expect(saveButton(view).disabled).toBe(true)
  expect(view.getByText(/Read the saved quota again/)).toBeTruthy()

  rereadFails = false
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  await waitFor(() => expect(saveButton(view).disabled).toBe(false))
  expect(saveButton(view).textContent).toBe('Save again')
  expect(input.value).toBe('512')
  expect(api.calls.filter((c) => c.method === 'GET').length).toBe(3)
})

test('an unsaved quota edit does not carry over to another project', async () => {
  const body = (projectId: number, quota: number) =>
    wrap(
      <AttachmentsSettingsBody
        projectId={projectId}
        usage={usage({ quota_bytes: quota * MIB })}
        usageReadAt={1}
        isPending={false}
        error={null}
        isRetrying={false}
        onRetry={() => {}}
      />
    )
  const view = render(body(3, 256))
  const input = () => view.getByLabelText('Quota (MiB)') as HTMLInputElement
  fireEvent.change(input(), { target: { value: '999' } })
  expect(input().value).toBe('999')

  view.rerender(body(4, 64))
  await waitFor(() => expect(input().value).toBe('64'))
})

test('Retry on a failed usage read reads it again', () => {
  const onRetry = mock(() => {})
  const view = render(
    wrap(
      <AttachmentsSettingsBody
        projectId={3}
        usage={undefined}
        usageReadAt={0}
        isPending={false}
        error={new Error('offline')}
        isRetrying={false}
        onRetry={onRetry}
      />
    )
  )
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  expect(onRetry).toHaveBeenCalledTimes(1)
})

test('Retry on a failed attachment list reads it again', () => {
  const onRetry = mock(() => {})
  const view = render(
    wrap(
      <AttachmentItems
        projectId={3}
        ownerLabel="event"
        items={undefined}
        isPending={false}
        error={new Error('offline')}
        isRetrying={false}
        onRetry={onRetry}
        usage={usage()}
        onDelete={() => {}}
      />
    )
  )
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  expect(onRetry).toHaveBeenCalledTimes(1)
})
