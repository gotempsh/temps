// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerEmailsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('emails send', () => {
  test('sends cc, bcc, reply-to, html and headers', async () => {
    const run = await runWithApi(
      registerEmailsCommands,
      [
        'emails', 'send', '--to', 'a@example.com', '--subject', 'Hi', '--body', 'Hello', '--from', 'app@example.com',
        '--cc', 'b@example.com', '--cc', 'c@example.com', '--bcc', 'd@example.com', '--reply-to', 'help@example.com',
        '--html', '<p>Hello</p>', '--headers', '{"X-Campaign":"spring"}', '--yes',
      ],
      { 'POST /emails': { id: 'e1' } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({
      to: ['a@example.com'],
      subject: 'Hi',
      text: 'Hello',
      from: 'app@example.com',
      cc: ['b@example.com', 'c@example.com'],
      bcc: ['d@example.com'],
      reply_to: 'help@example.com',
      html: '<p>Hello</p>',
      headers: { 'X-Campaign': 'spring' },
    })
  })

  test('headers that are not JSON fail before any request', async () => {
    const run = await runWithApi(registerEmailsCommands, ['emails', 'send', '--to', 'a@example.com', '--subject', 'Hi', '--body', 'x', '--headers', 'X: y', '--yes'], {})
    expect(run.error?.message).toMatch(/--headers/)
    expect(run.requests).toEqual([])
  })
})
