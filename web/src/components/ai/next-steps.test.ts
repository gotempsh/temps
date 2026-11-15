// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { answeredSince, latestAnswerKey, nextStepsRow } from './next-steps'

describe('latestAnswerKey', () => {
  test('keys the last finished answer', () => {
    expect(
      latestAnswerKey([
        { role: 'user', content: 'deploy it' },
        { role: 'assistant', content: 'Deployed.', server_cursor: 'c42' },
      ])
    ).toMatch(/^answer:9:[0-9a-f]+$/)
  })

  test('a live answer and its stored copy share a key', () => {
    const live = latestAnswerKey([
      { role: 'assistant', content: 'Deployed.', client_turn_id: 't1' },
    ])
    const stored = latestAnswerKey([
      {
        role: 'assistant',
        content: 'Deployed.\n',
        server_cursor: 'c42',
        created_at: '2026-10-19T10:00:00Z',
      },
    ])
    expect(live).toBe(stored)
  })

  test('different answers get different keys', () => {
    expect(
      latestAnswerKey([{ role: 'assistant', content: 'Deployed.' }])
    ).not.toBe(latestAnswerKey([{ role: 'assistant', content: 'Deleted.' }]))
  })

  test('is null while there is no answer to follow on from', () => {
    expect(latestAnswerKey([])).toBeNull()
    expect(latestAnswerKey([{ role: 'user', content: 'hi' }])).toBeNull()
    expect(
      latestAnswerKey([
        { role: 'assistant', content: '  ', server_cursor: 'c' },
      ])
    ).toBeNull()
  })
})

describe('nextStepsRow', () => {
  const base = {
    accepted: false,
    dismissed: false,
    answeredWhileOpen: true,
    hasSaved: false,
  }

  test('off shows nothing, even after a yes', () => {
    expect(nextStepsRow({ ...base, mode: 'off' })).toBe('hidden')
    expect(nextStepsRow({ ...base, mode: 'off', accepted: true })).toBe(
      'hidden'
    )
  })

  test('ask offers first and loads only after a yes', () => {
    expect(nextStepsRow({ ...base, mode: 'ask' })).toBe('offer')
    expect(nextStepsRow({ ...base, mode: 'ask', accepted: true })).toBe('load')
  })

  test('always loads for a new answer but only offers for an old one', () => {
    expect(nextStepsRow({ ...base, mode: 'always' })).toBe('load')
    expect(
      nextStepsRow({ ...base, mode: 'always', answeredWhileOpen: false })
    ).toBe('offer')
  })

  test('saved suggestions show without asking or loading', () => {
    expect(nextStepsRow({ ...base, mode: 'ask', hasSaved: true })).toBe('saved')
    expect(
      nextStepsRow({
        ...base,
        mode: 'always',
        hasSaved: true,
        answeredWhileOpen: false,
      })
    ).toBe('saved')
  })

  test('saved suggestions stay hidden when off or dismissed', () => {
    expect(nextStepsRow({ ...base, mode: 'off', hasSaved: true })).toBe(
      'hidden'
    )
    expect(
      nextStepsRow({ ...base, mode: 'ask', hasSaved: true, dismissed: true })
    ).toBe('hidden')
  })

  test('no hides the row for that answer', () => {
    expect(nextStepsRow({ ...base, mode: 'ask', dismissed: true })).toBe(
      'hidden'
    )
    expect(nextStepsRow({ ...base, mode: 'always', dismissed: true })).toBe(
      'hidden'
    )
  })
})

describe('answeredSince', () => {
  const openedAt = Date.parse('2026-10-19T10:00:00Z')

  test('an answer written after opening is new', () => {
    expect(
      answeredSince(
        [
          {
            role: 'assistant',
            content: 'x',
            created_at: '2026-10-19T10:05:00Z',
          },
        ],
        openedAt
      )
    ).toBe(true)
  })

  test('an answer read before opening is not', () => {
    expect(
      answeredSince(
        [
          {
            role: 'assistant',
            content: 'x',
            created_at: '2026-10-18T09:00:00Z',
          },
        ],
        openedAt
      )
    ).toBe(false)
  })

  test('an answer still streaming, without a timestamp, is new', () => {
    expect(answeredSince([{ role: 'assistant', content: 'x' }], openedAt)).toBe(
      true
    )
  })

  test('no answer means nothing new', () => {
    expect(answeredSince([{ role: 'user', content: 'hi' }], openedAt)).toBe(
      false
    )
  })
})
