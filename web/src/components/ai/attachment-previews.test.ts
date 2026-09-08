// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { createPendingAttachment } from './attachment-previews'

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
