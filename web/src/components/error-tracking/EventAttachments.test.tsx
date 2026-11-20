// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

import type { AttachmentUsage, AttachmentView } from '@/api/client/types.gen'
import { AttachmentItems } from './EventAttachments'
import {
  AttachmentsSettingsBody,
  AttachmentUsageSummary,
  QuotaForm,
} from './ErrorAttachmentsSettings'

const usage = (overrides: Partial<AttachmentUsage> = {}): AttachmentUsage => ({
  configured: true,
  reason: null,
  setup_path: null,
  used_bytes: 0,
  files: 0,
  quota_bytes: 268_435_456,
  max_attachment_bytes: 2_097_152,
  max_event_attachment_bytes: 4_194_304,
  max_attachments_per_event: 8,
  dropped_over_quota: 0,
  dropped_queue_full: 0,
  ...overrides,
})

const attachment = (overrides: Partial<AttachmentView>): AttachmentView => ({
  id: 41,
  owner_kind: 'error',
  owner_id: 9,
  filename: 'app.log',
  content_type: 'text/plain',
  attachment_type: 'event.attachment',
  size_bytes: 2048,
  created_at: '2026-11-20T10:00:00Z',
  ...overrides,
})

const render = (props: {
  items?: AttachmentView[]
  usage?: AttachmentUsage
  isPending?: boolean
  error?: unknown
  isRetrying?: boolean
}) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <AttachmentItems
        projectId={3}
        ownerLabel="event"
        items={props.items}
        isPending={props.isPending ?? false}
        error={props.error ?? null}
        isRetrying={props.isRetrying}
        onRetry={() => {}}
        usage={props.usage}
        onDelete={() => {}}
      />
    </MemoryRouter>
  )

describe('AttachmentItems', () => {
  test('without blob storage it says what is missing, shows an example and links the fix', () => {
    const html = render({
      items: [],
      usage: usage({
        configured: false,
        reason:
          'No blob storage is set up, so attachments sent by SDKs are dropped.',
        setup_path: '/storage',
      }),
    })
    expect(html).toContain('No blob storage is set up')
    expect(html).toContain('addAttachment')
    expect(html).toContain('href="/storage"')
    expect(html).toContain('Enable Blob storage')
  })

  test('shows images inline and offers every file as a download', () => {
    const html = render({
      items: [
        attachment({ id: 41 }),
        attachment({
          id: 42,
          filename: 'screenshot.png',
          content_type: 'image/png',
        }),
      ],
      usage: usage(),
    })
    expect(html).toContain(
      '<img src="/api/projects/3/error-attachments/42/content"'
    )
    expect(html).not.toContain(
      '<img src="/api/projects/3/error-attachments/41/content"'
    )
    expect(html).toContain(
      'href="/api/projects/3/error-attachments/41/content" download="app.log"'
    )
    expect(html).toContain('2.00 KiB')
    expect(html).toContain('aria-label="Delete screenshot.png"')
  })

  test('a failed first load says so and offers a retry', () => {
    const html = render({ error: new Error('offline'), usage: usage() })
    expect(html).toContain('The attachments could not be loaded.')
    expect(html).toContain('Retry')
    expect(html).not.toContain('No files were sent')
  })

  test('a failed refresh keeps the files already shown under its error', () => {
    const html = render({
      items: [attachment({ id: 41 })],
      error: new Error('offline'),
      usage: usage(),
    })
    expect(html).toContain('could not be refreshed')
    expect(html).toContain('app.log')
    expect(html).toContain('Retry')
  })

  test('a retry under way disables its button', () => {
    const html = render({
      error: new Error('offline'),
      isRetrying: true,
      usage: usage(),
    })
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Retrying…<\/button>/)
  })

  test('with storage and no files it explains how an SDK sends one', () => {
    const html = render({ items: [], usage: usage() })
    expect(html).toContain('No files were sent with this event')
    expect(html).toContain('addAttachment')
  })
})

describe('AttachmentUsageSummary', () => {
  test('shows the quota used and the drops since the server started', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <AttachmentUsageSummary
          usage={usage({
            used_bytes: 67_108_864,
            files: 3,
            dropped_over_quota: 2,
          })}
        />
      </MemoryRouter>
    )
    expect(html).toContain('64.0 MiB of 256 MiB used')
    expect(html).toContain('3 files')
    expect(html).toContain('2 dropped over quota')
    expect(html).toContain('2.00 MiB per file')
  })
})

describe('QuotaForm', () => {
  const renderForm = (quota_bytes: number) =>
    renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <QuotaForm projectId={3} usage={usage({ quota_bytes })} />
      </QueryClientProvider>
    )

  test('offers Save only once the quota is changed', () => {
    const html = renderForm(268_435_456)
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Save quota<\/button>/)
    expect(html).not.toContain('not a whole number of MiB')
  })

  test('a quota that is not whole MiB says so and can be saved as shown', () => {
    const html = renderForm(1)
    expect(html).toContain('The current quota, 1 B, is not a whole number')
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*>Save quota/)
  })
})

describe('AttachmentsSettingsBody', () => {
  const renderBody = (props: { usage?: AttachmentUsage; error?: unknown }) =>
    renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <AttachmentsSettingsBody
            projectId={3}
            usage={props.usage}
            isPending={false}
            error={props.error ?? null}
            isRetrying={false}
            onRetry={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )

  test('a failed refresh keeps the quota form under its error', () => {
    const html = renderBody({ usage: usage(), error: new Error('offline') })
    expect(html).toContain('could not be refreshed')
    expect(html).toContain('Retry')
    expect(html).toContain('id="attachment-quota"')
    expect(html).toContain('of 256 MiB used')
  })

  test('a failed first load shows only the error and a retry', () => {
    const html = renderBody({ error: new Error('offline') })
    expect(html).toContain('Attachment usage could not be loaded.')
    expect(html).toContain('Retry')
    expect(html).not.toContain('id="attachment-quota"')
  })
})
