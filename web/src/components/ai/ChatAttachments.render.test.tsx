// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ChatAttachments } from './DebugChatPanel'

const image = {
  id: 'att_test',
  name: 'diagram.png',
  is_image: true,
  mime_type: 'image/png',
  size_bytes: 100,
  sandbox_path: '/unused',
}

describe('ChatAttachments', () => {
  test('pending images have separate expand and remove buttons', () => {
    const html = renderToStaticMarkup(
      <ChatAttachments
        attachments={[{ ...image, preview_url: 'blob:test' }]}
        onRemove={() => {}}
      />
    )
    expect(html).toContain('aria-label="Expand diagram.png"')
    expect(html).toContain('aria-haspopup="dialog"')
    expect(html).toContain('aria-label="Remove diagram.png"')
    expect(html).not.toMatch(
      /<button[^>]*>[^]*?<button[^>]*>[^]*?<\/button>[^]*?<\/button>/
    )
  })
  test('persisted images open an in-app dialog through the protected content endpoint', () => {
    const html = renderToStaticMarkup(
      <ChatAttachments
        attachments={[image]}
        contentBase="/api/ai/conversations/test/attachments"
      />
    )
    expect(html).toContain('aria-label="Expand diagram.png"')
    expect(html).toContain('/att_test?name=diagram.png')
    expect(html).not.toContain('target="_blank"')
  })
  test('ordinary files retain their download link', () => {
    const html = renderToStaticMarkup(
      <ChatAttachments
        attachments={[{ ...image, name: 'notes.txt', is_image: false }]}
        contentBase="/api/ai/conversations/test/attachments"
      />
    )
    expect(html).toContain('aria-label="Open notes.txt"')
    expect(html).not.toContain('aria-haspopup="dialog"')
  })
  test('sent images ignore revoked composer blobs', () => {
    const html = renderToStaticMarkup(
      <ChatAttachments
        attachments={[{ ...image, preview_url: 'blob:revoked-after-send' }]}
        contentBase="/api/ai/conversations/test/attachments"
      />
    )
    expect(html).not.toContain('blob:revoked-after-send')
    expect(html).toContain('/att_test?name=diagram.png')
  })
})
