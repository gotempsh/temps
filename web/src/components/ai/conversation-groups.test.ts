// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  chatDateBucket,
  filterChats,
  groupChatsByDate,
  type GroupableChat,
} from './conversation-groups'

// A fixed local "now" so the calendar-day buckets are deterministic.
const NOW = new Date(2026, 10, 15, 14, 30)
const at = (daysAgo: number, hour = 12) =>
  new Date(2026, 10, 15 - daysAgo, hour).toISOString()

const chat = (
  id: string,
  lastActivityAt: string,
  extra: Partial<GroupableChat> = {}
): GroupableChat => ({
  public_id: id,
  last_activity_at: lastActivityAt,
  ...extra,
})

describe('chatDateBucket', () => {
  test('uses calendar days, not rolling 24-hour windows', () => {
    // Just after midnight today is today; just before is yesterday.
    expect(
      chatDateBucket(new Date(2026, 10, 15, 0, 1).toISOString(), NOW)
    ).toBe('today')
    expect(
      chatDateBucket(new Date(2026, 10, 14, 23, 59).toISOString(), NOW)
    ).toBe('yesterday')
  })

  test('files older activity into week, month and older', () => {
    expect(chatDateBucket(at(3), NOW)).toBe('week')
    expect(chatDateBucket(at(20), NOW)).toBe('month')
    expect(chatDateBucket(at(90), NOW)).toBe('older')
  })

  test('treats future timestamps as today and bad ones as older', () => {
    expect(chatDateBucket(at(-1), NOW)).toBe('today')
    expect(chatDateBucket('not a date', NOW)).toBe('older')
  })

  test('a 25-hour daylight-saving day still starts at local midnight', () => {
    // Central European clocks went back on 2026-10-25, so that day lasted 25
    // hours. Subtracting 24 hours from the next midnight lands at 01:00 and
    // files 00:30 that day under "Previous 7 days" instead of "Yesterday".
    const previousTz = process.env.TZ
    process.env.TZ = 'Europe/Madrid'
    try {
      const now = new Date(2026, 9, 26, 10, 0)
      const halfPastMidnight = new Date(2026, 9, 25, 0, 30).toISOString()
      expect(
        new Date(2026, 9, 26).getTime() - new Date(2026, 9, 25).getTime()
      ).toBe(25 * 60 * 60 * 1000)
      expect(chatDateBucket(halfPastMidnight, now)).toBe('yesterday')
      const lateOnTheTwentyFourth = new Date(2026, 9, 24, 23, 30).toISOString()
      expect(chatDateBucket(lateOnTheTwentyFourth, now)).toBe('week')
    } finally {
      if (previousTz === undefined) delete process.env.TZ
      else process.env.TZ = previousTz
    }
  })
})

describe('groupChatsByDate', () => {
  test('keeps newest-first order within each heading, headings in time order', () => {
    const groups = groupChatsByDate(
      [
        chat('a', at(0, 13)),
        chat('b', at(0, 9)),
        chat('c', at(1)),
        chat('d', at(4)),
        chat('e', at(120)),
      ],
      NOW
    )
    expect(groups.map((g) => g.label)).toEqual([
      'Today',
      'Yesterday',
      'Previous 7 days',
      'Older',
    ])
    expect(groups[0].chats.map((c) => c.public_id)).toEqual(['a', 'b'])
  })

  test('omits empty headings and returns nothing for no chats', () => {
    expect(groupChatsByDate([], NOW)).toEqual([])
    const groups = groupChatsByDate([chat('a', at(40))], NOW)
    expect(groups).toHaveLength(1)
    expect(groups[0].bucket).toBe('older')
  })

  test('never drops a chat', () => {
    const chats = [at(0), at(1), at(5), at(25), at(400), 'bad'].map((t, i) =>
      chat(String(i), t)
    )
    const total = groupChatsByDate(chats, NOW).reduce(
      (sum, group) => sum + group.chats.length,
      0
    )
    expect(total).toBe(chats.length)
  })
})

describe('filterChats', () => {
  const chats = [
    chat('a', at(0), { title: 'Diagnose GitHub 401 error' }),
    chat('b', at(0), { title: 'Traffic report', project_name: 'checkout-api' }),
    chat('c', at(0), {
      title: 'Untitled',
      workspace: { public_id: 'app_1', name: 'Todo list' },
    }),
    chat('d', at(0), { title: null }),
  ]

  test('an empty or blank query keeps every chat', () => {
    expect(filterChats(chats, '')).toHaveLength(4)
    expect(filterChats(chats, '   ')).toHaveLength(4)
  })

  test('matches title, project and workspace, ignoring case', () => {
    expect(filterChats(chats, 'github').map((c) => c.public_id)).toEqual(['a'])
    expect(filterChats(chats, 'CHECKOUT').map((c) => c.public_id)).toEqual([
      'b',
    ])
    expect(filterChats(chats, 'todo').map((c) => c.public_id)).toEqual(['c'])
  })

  test('every word must match somewhere', () => {
    expect(
      filterChats(chats, 'traffic checkout').map((c) => c.public_id)
    ).toEqual(['b'])
    expect(filterChats(chats, 'traffic github')).toEqual([])
  })
})
