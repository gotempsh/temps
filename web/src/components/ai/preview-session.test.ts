// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  previewFrameSource,
  type CreatePreviewSession,
} from './preview-session'

const page = 'http://ws-abc-3000.localho.st:8055/pricing'

function fake(
  answer: Awaited<ReturnType<CreatePreviewSession>>,
  calls: unknown[] = []
): CreatePreviewSession {
  return async (options) => {
    calls.push(options.body)
    return answer
  }
}

describe('previewFrameSource', () => {
  test('frames the session redirect for a Temps-login preview', async () => {
    const calls: unknown[] = []
    const src = await previewFrameSource(
      'sbx_abc',
      page,
      fake(
        {
          data: {
            redirect_url:
              'http://ws-abc-3000.localho.st:8055/.well-known/temps/ingress/grant?g=1',
          },
          response: new Response(null, { status: 201 }),
        },
        calls
      )
    )
    expect(src).toBe(
      'http://ws-abc-3000.localho.st:8055/.well-known/temps/ingress/grant?g=1'
    )
    expect(calls).toEqual([{ sandbox_id: 'sbx_abc', url: page }])
  })

  test('frames the page itself when the preview does not ask for a login', async () => {
    const src = await previewFrameSource(
      'sbx_abc',
      page,
      fake({
        error: { status: 409, detail: 'public preview' },
        response: new Response(null, { status: 409 }),
      })
    )
    expect(src).toBe(page)
  })

  test('surfaces the server explanation instead of framing a sign-in page', async () => {
    await expect(
      previewFrameSource(
        'sbx_abc',
        page,
        fake({
          error: {
            status: 403,
            detail: 'Only the sandbox owner can open its preview',
          },
          response: new Response(null, { status: 403 }),
        })
      )
    ).rejects.toThrow('Only the sandbox owner can open its preview')
  })
})
