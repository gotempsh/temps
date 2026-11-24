// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  ASSISTANT_ATTACHMENT_LIMITS,
  assistantAttachTitle,
  assistantImageError,
  attachHint,
  attachmentSelectionError,
  createPendingAttachment,
} from './attachment-previews'

describe('attachmentSelectionError', () => {
  const file = { name: 'notes.txt', size: 20 * 1024 * 1024 }
  it('allows images and files up to the exact size and count limits', () => {
    expect(attachmentSelectionError([file], 7)).toBeNull()
  })
  it('rejects excess selections instead of silently truncating them', () => {
    expect(attachmentSelectionError([file, file], 7)).toContain('8 files')
  })
  it('identifies an oversized file before uploading the selection', () => {
    expect(
      attachmentSelectionError([{ ...file, size: file.size + 1 }], 0)
    ).toContain('notes.txt')
  })
})

describe('assistant attachments', () => {
  const limits = ASSISTANT_ATTACHMENT_LIMITS
  const note = { name: 'notes.txt', size: limits.maxBytes }

  it('holds assistant chats to the smaller limits the server enforces', () => {
    expect(
      attachmentSelectionError([note], limits.maxFiles - 1, limits)
    ).toBeNull()
    expect(
      attachmentSelectionError([note, note], limits.maxFiles - 1, limits)
    ).toContain(`${limits.maxFiles} files`)
    expect(
      attachmentSelectionError([{ ...note, size: note.size + 1 }], 0, limits)
    ).toContain('10 MB')
  })

  it('holds images to less than other files', () => {
    const png = { name: 'shot.png', type: 'image/png', size: 6 * 1024 * 1024 }
    const pdf = { name: 'doc.pdf', type: 'application/pdf', size: png.size }
    expect(attachmentSelectionError([png], 0, limits)).toContain(
      'shot.png exceeds the 5 MB limit for images'
    )
    expect(attachmentSelectionError([pdf], 0, limits)).toBeNull()
  })

  it('refuses an image before upload when the model cannot read images', () => {
    const png = { name: 'shot.png', type: 'image/png' }
    const text = { name: 'notes.txt', type: 'text/plain' }
    expect(assistantImageError([text, png], 'no')).toContain('shot.png')
    expect(assistantImageError([text, png], 'no')).toContain('Pick a model')
    expect(assistantImageError([text], 'no')).toBeNull()
    expect(assistantImageError([png], 'yes')).toBeNull()
    // The server decides when the catalogue is silent.
    expect(assistantImageError([png], 'unknown')).toBeNull()
  })
})

describe('createPendingAttachment', () => {
  it('does not retain or leak a preview when an upload resolves after unmount', () => {
    const revoked: string[] = []
    let mountedCheck = 0
    const result = createPendingAttachment(
      {
        id: 'attachment-1',
        name: 'image.png',
        mime_type: 'image/png',
        size_bytes: 10,
        sandbox_path: '/tmp/image.png',
        is_image: true,
      },
      new File(['image'], 'image.png', { type: 'image/png' }),
      () => ++mountedCheck === 1,
      {
        createObjectURL: () => 'blob:late-upload',
        revokeObjectURL: (url) => revoked.push(url),
      }
    )

    expect(result).toBeNull()
    expect(revoked).toEqual(['blob:late-upload'])
  })

  it('does not create a preview when the composer already unmounted', () => {
    let created = false
    const result = createPendingAttachment(
      {
        id: 'attachment-1',
        name: 'image.png',
        mime_type: 'image/png',
        size_bytes: 10,
        sandbox_path: '/tmp/image.png',
        is_image: true,
      },
      new File(['image'], 'image.png', { type: 'image/png' }),
      () => false,
      {
        createObjectURL: () => {
          created = true
          return 'blob:unexpected'
        },
        revokeObjectURL: () => undefined,
      }
    )

    expect(result).toBeNull()
    expect(created).toBe(false)
  })
})

describe('attach copy for assistant chats', () => {
  const limits = ASSISTANT_ATTACHMENT_LIMITS
  it('says images are unavailable when the model is text-only', () => {
    expect(attachHint(true, 'no', limits)).toContain('PDFs and files')
    expect(attachHint(true, 'no', limits)).toContain('can\u2019t read images')
    expect(assistantAttachTitle('no', limits, true)).toContain(
      'Attach PDFs and files'
    )
    expect(assistantAttachTitle('no', limits, true)).toContain(
      'other files it sees by name and size only'
    )
  })
  it('offers images when the model reads them', () => {
    expect(attachHint(true, 'yes', limits)).toContain('images, PDFs, and files')
    expect(assistantAttachTitle('yes', limits, true)).toContain('10 MB')
  })
  it('warns that images depend on the model when the catalogue is silent', () => {
    expect(assistantAttachTitle('unknown', limits, true)).toContain(
      'only work if the model can read them'
    )
  })
  it('asks for a conversation first', () => {
    expect(assistantAttachTitle('yes', limits, false)).toContain(
      'first message'
    )
  })
  it('keeps the workspace wording for non-assistant chats', () => {
    expect(
      attachHint(false, 'unknown', { maxFiles: 8, maxBytes: 20 * 1024 * 1024 })
    ).toContain('up to 8 files, 20 MB each')
  })
})
