// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type { AttachmentUsage, AttachmentView } from '@/api/client/types.gen'
import { AttachmentItems } from './EventAttachments'
import { AttachmentUsageSummary } from './ErrorAttachmentsSettings'

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
}) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <AttachmentItems
        projectId={3}
        ownerLabel="event"
        items={props.items}
        isPending={props.isPending ?? false}
        error={null}
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
